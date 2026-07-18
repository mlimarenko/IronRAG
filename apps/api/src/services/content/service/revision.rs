use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt, stream};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    agent_runtime::{task::RuntimeTask, tasks::graph_extract::GraphExtractTask},
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::catalog::ChunkingTemplate,
    domains::content::{ContentDocument, ContentDocumentHead, ContentRevision},
    domains::knowledge::TypedTechnicalFact,
    domains::ops::ASYNC_OP_STATUS_READY,
    domains::provider_profiles::ProviderModelSelection,
    domains::recognition::LibraryRecognitionPolicy,
    infra::knowledge_rows::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
        KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
    },
    infra::repositories::{
        self as repositories, catalog_repository,
        content_repository::{
            self, CreateContentRevisionOutcome, NewContentDocument, NewContentDocumentHead,
            NewContentRevisionProjection, PromoteDocumentHeadOutcome,
        },
    },
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        content::error::ContentServiceError,
        content::source_access::{
            derive_content_source_file_name, derive_storage_backed_content_file_name,
        },
        content::storage::ContentStorageService,
        graph::extract::{
            GraphExtractionExecutionError, GraphExtractionOutcome, GraphExtractionSubTypeHintEntry,
            GraphExtractionSubTypeHintGroup, GraphExtractionSubTypeHints,
            build_graph_extraction_cache_fingerprint, canonical_graph_extraction_normalized_json,
            extract_chunk_graph_candidates, extraction_lifecycle_from_record,
            repair_graph_extraction_candidate_set,
        },
        graph::projection::resolve_projection_scope,
        ingest::cancellation::ensure_not_cancelled,
        ingest::runtime::resolve_runtime_task_execution_context,
        ingest::service::{
            INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH, LeaseAttemptCommand,
            RecordStageUnitProgressCommand,
        },
        ingest::structured_preparation::{
            PrepareStructuredRevisionCommand, StructuredPreparationService,
        },
        ingest::technical_facts::ExtractTechnicalFactsCommand,
        ops::billing::CaptureGraphExtractionBillingCommand,
        query::vector_dimensions::invalidate_library_embedding_profile_inventory,
    },
    shared::extraction::file_extract::{
        FileExtractError, FileExtractionPlan, FileExtractionRequest, UploadAdmissionError,
        UploadFileKind, append_docling_picture_ocr_fallback,
        augment_docling_output_with_vision_picture_ocr, build_docling_pdf_extraction_plan,
        build_runtime_file_extraction_plan, docling_embedded_picture_vision_enabled,
        validate_upload_file_admission,
    },
    shared::extraction::{
        ExtractionOutput,
        record_jsonl::RECORD_JSONL_SOURCE_FORMAT,
        structured_document::StructuredBlockKind,
        table_graph::{TableGraphProfile, build_table_graph_profile},
        table_summary::{is_table_summary_text, parse_table_column_summary},
    },
};

use super::pipeline::{
    GraphExtractionChunkPolicy, build_canonical_graph_extraction_request,
    build_graph_chunk_content, typed_fact_supports_chunk,
};

const RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT: usize = 12;
use super::{
    ContentMutationAdmission, ContentService, CreateDocumentCommand, CreateRevisionCommand,
    EditableDocumentContext, InlineMutationContext, MaterializeRevisionGraphCandidatesCommand,
    PendingChunkInsert, PreparedRevisionPersistenceSummary, PromoteHeadCommand,
    ReprocessRevisionSource, RevisionGraphCandidateMaterialization, locate_chunk_offsets,
    map_revision_row, map_structured_revision_data, map_structured_revision_row, sha256_hex_text,
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

async fn resolve_library_recognition_policy(
    state: &AppState,
    library_id: Uuid,
) -> Result<LibraryRecognitionPolicy, String> {
    let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
        .await
        .map_err(|error| format!("failed to load library recognition policy: {error}"))?
        .ok_or_else(|| format!("library {library_id} was not found"))?;
    LibraryRecognitionPolicy::from_json(row.recognition_policy)
}

fn rendered_revision_text_source_file_name(revision: &ContentRevision) -> String {
    let fallback = format!("revision-{}.txt", revision.id);
    let base = derive_content_source_file_name(
        revision.source_uri.as_deref(),
        revision.title.as_deref(),
        &fallback,
    );
    let stem = base.rsplit_once('.').map_or(base.as_str(), |(stem, _)| stem).trim();
    let normalized_stem = if stem.is_empty() { "revision" } else { stem };
    format!("{normalized_stem}.txt")
}

fn record_stream_reprocess_file_name(revision: &ContentRevision) -> String {
    let fallback = format!("revision-{}.jsonl", revision.id);
    let base = derive_content_source_file_name(
        revision.source_uri.as_deref(),
        revision.title.as_deref(),
        &fallback,
    );
    let trimmed = base.trim();
    let extension = trimmed.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase());
    if matches!(extension.as_deref(), Some("jsonl" | "ndjson")) {
        return trimmed.to_string();
    }
    let stem = trimmed.rsplit_once('.').map_or(trimmed, |(stem, _)| stem).trim();
    let normalized_stem = if stem.is_empty() { "revision" } else { stem };
    format!("{normalized_stem}.jsonl")
}

fn should_sync_stage_unit_progress(completed_units: usize, total_units: usize) -> bool {
    if total_units <= 20 || completed_units >= total_units {
        return true;
    }
    let stride = (total_units / 20).max(1);
    completed_units.is_multiple_of(stride)
}

async fn sync_extract_graph_stage_progress(
    state: &AppState,
    attempt_id: Option<Uuid>,
    completed_units: usize,
    total_units: usize,
) {
    let Some(attempt_id) = attempt_id else {
        return;
    };
    if total_units == 0 {
        return;
    }
    let command = RecordStageUnitProgressCommand {
        attempt_id,
        stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
        completed_units: u32::try_from(completed_units).unwrap_or(u32::MAX),
        total_units: u32::try_from(total_units).unwrap_or(u32::MAX),
        details_json: serde_json::json!({}),
    };
    if let Err(error) =
        state.canonical_services.ingest.record_stage_unit_progress(state, command).await
    {
        warn!(
            attempt_id = %attempt_id,
            ?error,
            "failed to sync extract_graph stage progress"
        );
    }
}

async fn reusable_prepared_revision_summary(
    state: &AppState,
    revision: &content_repository::ContentRevisionRow,
    extraction_plan: &FileExtractionPlan,
) -> Result<Option<PreparedRevisionPersistenceSummary>, ContentServiceError> {
    let revision_id = revision.id;
    let Some(structured_revision) = state
        .document_store
        .get_structured_revision(revision_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    else {
        return Ok(None);
    };
    let artifact_identity_matches = structured_revision.revision_id == revision_id
        && structured_revision.document_id == revision.document_id
        && structured_revision.workspace_id == revision.workspace_id
        && structured_revision.library_id == revision.library_id;
    let extraction_shape_matches = structured_revision.normalization_profile
        == extraction_plan.normalization_profile
        && structured_revision.source_format
            == extraction_plan.source_format_metadata.source_format;
    let is_prepared_candidate = structured_revision.preparation_state == "prepared"
        && structured_revision.chunk_count > 0
        && artifact_identity_matches
        && extraction_shape_matches;
    if !is_prepared_candidate {
        warn!(
            revision_id = %revision_id,
            document_id = %revision.document_id,
            library_id = %revision.library_id,
            artifact_preparation_state = %structured_revision.preparation_state,
            artifact_chunk_count = structured_revision.chunk_count,
            artifact_identity_matches,
            extraction_shape_matches,
            "structured revision persistence resume skipped existing artifact; rebuilding prepared structure"
        );
        return Ok(None);
    }

    let expected_chunk_count = i64::from(structured_revision.chunk_count);
    let expected_fact_count = i64::from(structured_revision.typed_fact_count.max(0));
    let postgres_chunk_count =
        content_repository::count_chunks_by_revision(&state.persistence.postgres, revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let knowledge_chunk_count = state
        .document_store
        .count_chunks_by_revision(revision_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let knowledge_fact_count = state
        .document_store
        .count_technical_facts_by_revision(revision_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let artifact_counts_match = postgres_chunk_count == expected_chunk_count
        && knowledge_chunk_count == expected_chunk_count
        && knowledge_fact_count == expected_fact_count;
    if !artifact_counts_match {
        warn!(
            revision_id = %revision_id,
            document_id = %revision.document_id,
            library_id = %revision.library_id,
            expected_chunk_count,
            postgres_chunk_count,
            knowledge_chunk_count,
            expected_fact_count,
            knowledge_fact_count,
            "structured revision persistence resume found incomplete artifacts; rebuilding prepared structure"
        );
        return Ok(None);
    }

    let normalization_profile = structured_revision.normalization_profile.clone();
    let prepared_revision = map_structured_revision_row(structured_revision);
    info!(
        revision_id = %revision_id,
        document_id = %revision.document_id,
        library_id = %revision.library_id,
        chunk_count = expected_chunk_count,
        technical_fact_count = expected_fact_count,
        "structured revision persistence resume reused prepared artifacts"
    );
    Ok(Some(PreparedRevisionPersistenceSummary {
        prepared_revision,
        chunk_count: usize::try_from(expected_chunk_count).unwrap_or(usize::MAX),
        technical_fact_count: usize::try_from(expected_fact_count).unwrap_or(usize::MAX),
        technical_conflict_count: 0,
        normalization_profile,
        prepare_structure_elapsed_ms: 0,
        chunk_content_elapsed_ms: 0,
        extract_technical_facts_elapsed_ms: 0,
    }))
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
        let recognition_policy =
            resolve_library_recognition_policy(state, library_id).await.map_err(|error| {
                UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    &FileExtractError::ExtractionFailed {
                        file_kind: validate_upload_file_admission(
                            Some(file_name),
                            mime_type,
                            file_bytes,
                        )
                        .unwrap_or(UploadFileKind::Binary),
                        message: error,
                    },
                )
            })?;
        let vision_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractText)
            .await
            .map_err(|error| {
                UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    &FileExtractError::ExtractionFailed {
                        file_kind: validate_upload_file_admission(
                            Some(file_name),
                            mime_type,
                            file_bytes,
                        )
                        .unwrap_or(UploadFileKind::Binary),
                        message: format!(
                            "failed to resolve document-understanding binding: {error}"
                        ),
                    },
                )
            })?;
        let vision_provider = vision_binding.as_ref().map(|binding| ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        });
        let plan = build_runtime_file_extraction_plan(FileExtractionRequest {
            gateway: state.llm_gateway.as_ref(),
            vision_provider: vision_provider.as_ref(),
            vision_api_key: vision_binding.as_ref().and_then(|binding| binding.api_key.as_deref()),
            vision_base_url: vision_binding
                .as_ref()
                .and_then(|binding| binding.provider_base_url.as_deref()),
            vision_extra_parameters_json: vision_binding
                .as_ref()
                .map(|binding| &binding.extra_parameters_json),
            file_name: Some(file_name),
            mime_type,
            file_bytes: file_bytes.to_vec(),
            recognition_policy: &recognition_policy,
        })
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

    pub async fn build_runtime_pdf_docling_extraction_plan_from_output(
        &self,
        state: &AppState,
        library_id: Uuid,
        file_name: &str,
        mime_type: Option<&str>,
        file_size_bytes: u64,
        mut output: ExtractionOutput,
    ) -> Result<FileExtractionPlan, UploadAdmissionError> {
        self.augment_runtime_docling_output(state, library_id, &mut output).await.map_err(
            |error| {
                UploadAdmissionError::from_file_extract_error(
                    file_name,
                    mime_type,
                    file_size_bytes,
                    &error,
                )
            },
        )?;

        let plan = build_docling_pdf_extraction_plan(output);
        validate_extraction_plan(file_name, mime_type, file_size_bytes, &plan)?;
        Ok(plan)
    }

    pub async fn augment_runtime_docling_output(
        &self,
        state: &AppState,
        library_id: Uuid,
        output: &mut ExtractionOutput,
    ) -> Result<(), FileExtractError> {
        let recognition_policy = match resolve_library_recognition_policy(state, library_id).await {
            Ok(policy) => policy,
            Err(error) => {
                output.extracted_images.clear();
                return Err(FileExtractError::ExtractionFailed {
                    file_kind: UploadFileKind::Pdf,
                    message: format!(
                        "failed to load recognition policy for docling embedded picture OCR: {error}"
                    ),
                });
            }
        };
        let vision_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractText)
            .await
            .map_err(|error| {
                output.extracted_images.clear();
                FileExtractError::ExtractionFailed {
                    file_kind: UploadFileKind::Pdf,
                    message: format!("failed to resolve document-understanding binding: {error}"),
                }
            })?;
        if !docling_embedded_picture_vision_enabled(&recognition_policy, vision_binding.is_some()) {
            append_docling_picture_ocr_fallback(output);
            output.extracted_images.clear();
            return Ok(());
        }
        let Some(vision_binding) = vision_binding else {
            output.extracted_images.clear();
            return Ok(());
        };
        let vision_provider = ProviderModelSelection {
            provider_kind: vision_binding.provider_kind.clone(),
            model_name: vision_binding.model_name.clone(),
        };
        augment_docling_output_with_vision_picture_ocr(
            output,
            UploadFileKind::Pdf,
            state.llm_gateway.as_ref(),
            Some(&vision_provider),
            vision_binding.api_key.as_deref(),
            vision_binding.provider_base_url.as_deref(),
            Some(&vision_binding.extra_parameters_json),
        )
        .await
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

        let storage_update = content_repository::update_revision_storage_key(
            &state.persistence.postgres,
            revision_id,
            Some(&storage_key),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match storage_update {
            content_repository::UpdateRevisionStorageKeyOutcome::Updated(_) => {}
            content_repository::UpdateRevisionStorageKeyOutcome::RevisionNotFound => {
                return Err(ApiError::resource_not_found("revision", revision_id));
            }
            content_repository::UpdateRevisionStorageKeyOutcome::KnowledgeProjectionNotFound => {
                return Err(ApiError::service_unavailable(
                    "revision projection is converging; retry the storage update shortly",
                    "document_projection_converging",
                ));
            }
        }
        Ok(Some(storage_key))
    }

    pub async fn render_revision_text_source(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Option<String>, ApiError> {
        let revision = state
            .document_store
            .get_revision(revision_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        Ok(revision.and_then(|row| {
            row.normalized_text
                .map(|text| text.trim_end().to_string())
                .filter(|text| !text.is_empty())
        }))
    }

    pub async fn resolve_reprocess_revision_source(
        &self,
        state: &AppState,
        revision: &ContentRevision,
    ) -> Result<Option<ReprocessRevisionSource>, ApiError> {
        let storage_key = if let Some(storage_key) = revision
            .storage_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
        {
            Some(storage_key)
        } else {
            self.resolve_revision_storage_key(state, revision.id).await?
        };
        if let Some(storage_key) = storage_key {
            return Ok(Some(ReprocessRevisionSource {
                checksum: revision.checksum.clone(),
                mime_type: revision.mime_type.clone(),
                byte_size: revision.byte_size,
                title: revision.title.clone(),
                source_uri: revision.source_uri.clone(),
                storage_key,
            }));
        }

        if let Some(source) = self.resolve_record_stream_reprocess_source(state, revision).await? {
            return Ok(Some(source));
        }

        let Some(text_source) = self.render_revision_text_source(state, revision.id).await? else {
            return Ok(None);
        };
        let checksum = format!("sha256:{}", sha256_hex_text(&text_source));
        let file_name = rendered_revision_text_source_file_name(revision);
        let storage_key = self
            .persist_inline_file_source_with_storage_lock(
                state,
                revision.workspace_id,
                revision.library_id,
                &file_name,
                &checksum,
                text_source.as_bytes(),
            )
            .await?;

        Ok(Some(ReprocessRevisionSource {
            checksum,
            mime_type: "text/plain".to_string(),
            byte_size: i64::try_from(text_source.len()).unwrap_or(i64::MAX),
            title: Some(file_name),
            source_uri: Some(format!("derived-text://{}", revision.id)),
            storage_key,
        }))
    }

    async fn resolve_record_stream_reprocess_source(
        &self,
        state: &AppState,
        revision: &ContentRevision,
    ) -> Result<Option<ReprocessRevisionSource>, ApiError> {
        let Some(structured_revision) = state
            .document_store
            .get_structured_revision(revision.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };
        if structured_revision.source_format != RECORD_JSONL_SOURCE_FORMAT {
            return Ok(None);
        }

        let blocks = state
            .document_store
            .list_structured_blocks_by_revision(revision.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut lines = Vec::<String>::new();
        for block in blocks {
            if block.block_kind != StructuredBlockKind::SourceUnit.as_str() {
                continue;
            }
            let text = block.normalized_text.trim();
            if text.is_empty() {
                continue;
            }
            let payload = serde_json::json!({
                "id": block.block_id.to_string(),
                "kind": StructuredBlockKind::SourceUnit.as_str(),
                "ordinal": block.ordinal,
                "text": text,
            });
            let line = serde_json::to_string(&payload)
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            lines.push(line);
        }
        if lines.is_empty() {
            return Ok(None);
        }

        let payload = format!("{}\n", lines.join("\n"));
        let bytes = payload.as_bytes();
        let checksum = format!("sha256:{}", sha256_hex_text(&payload));
        let file_name = record_stream_reprocess_file_name(revision);
        let storage_key = self
            .persist_inline_file_source_with_storage_lock(
                state,
                revision.workspace_id,
                revision.library_id,
                &file_name,
                &checksum,
                bytes,
            )
            .await?;

        Ok(Some(ReprocessRevisionSource {
            checksum,
            mime_type: "application/x-ndjson".to_string(),
            byte_size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            title: Some(file_name),
            source_uri: Some(format!("derived-record-stream://{}", revision.id)),
            storage_key,
        }))
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
            .map_or_else(|| Uuid::now_v7().to_string(), ToString::to_string);
        let parent_external_key = command
            .parent_external_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        // Resolve the declared parent key to a concrete parent document when the
        // sibling already exists in this library. When it does not exist yet
        // (out-of-order connector sync), the key is recorded as pending and the
        // deferred resolver reconciles it later.
        let parent_document_id = match parent_external_key.as_deref() {
            Some(key) => content_repository::get_document_by_external_key(
                &state.persistence.postgres,
                command.library_id,
                key,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .map(|row| row.id),
            None => None,
        };
        // The role is finalized once a revision lands and its media class is
        // known (promote path / backfill). At shell-creation the media class is
        // unknown, so a child gets the provisional peer role `attachment`: never
        // a wrong demotion to `attached_context`, and `primary` when it has no
        // declared parent at all.
        let document_role =
            crate::domains::content::derive_document_role(parent_external_key.is_some(), false);
        let row = content_repository::create_document_with_projection(
            &state.persistence.postgres,
            &NewContentDocument {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                external_key: &external_key,
                document_state: "active",
                created_by_principal_id: command.created_by_principal_id,
                parent_external_key: parent_external_key.as_deref(),
                parent_document_id,
                document_role,
            },
            command.file_name.as_deref(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(ContentDocument {
            id: row.id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            external_key: row.external_key.clone(),
            document_state: row.document_state.clone(),
            created_at: row.created_at,
        })
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
        let outcome = content_repository::create_revision_with_projection(
            &state.persistence.postgres,
            &NewContentRevisionProjection {
                document_id: document.id,
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                content_source_kind: &command.content_source_kind,
                checksum: &command.checksum,
                mime_type: &command.mime_type,
                byte_size: command.byte_size,
                title: command.title.as_deref(),
                language_code: command.language_code.as_deref(),
                source_uri: command.source_uri.as_deref(),
                document_hint: command.document_hint.as_deref(),
                storage_key: command.storage_key.as_deref(),
                created_by_principal_id: command.created_by_principal_id,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match outcome {
            CreateContentRevisionOutcome::Created(row) => Ok(map_revision_row(*row)),
            CreateContentRevisionOutcome::DocumentNotFound => {
                Err(ApiError::resource_not_found("document", command.document_id))
            }
            CreateContentRevisionOutcome::DocumentDeleted => Err(ApiError::BadRequest(
                "deleted documents do not accept new revisions".to_string(),
            )),
            CreateContentRevisionOutcome::ProjectionUnavailable => {
                Err(ApiError::service_unavailable(
                    "document projection is converging; retry the revision shortly",
                    "document_projection_converging",
                ))
            }
        }
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

        let outcome = content_repository::promote_document_head_with_projection(
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
        let row = match outcome {
            PromoteDocumentHeadOutcome::Promoted(row) => row,
            PromoteDocumentHeadOutcome::DocumentNotFound => {
                return Err(ApiError::resource_not_found("document", command.document_id));
            }
            PromoteDocumentHeadOutcome::RevisionNotFound => {
                return Err(ApiError::service_unavailable(
                    "document projection is converging; retry the promotion shortly",
                    "document_projection_converging",
                ));
            }
            integrity @ PromoteDocumentHeadOutcome::ReferenceIntegrityViolation => {
                return Err(ApiError::internal_with_log(
                    integrity,
                    "document head reference integrity failure",
                ));
            }
        };
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

    pub(crate) async fn persist_inline_file_source_with_storage_lock(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        file_name: &str,
        checksum: &str,
        file_bytes: &[u8],
    ) -> Result<String, ApiError> {
        let storage_lock = content_repository::acquire_content_library_storage_lock(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let storage_key = self
            .persist_inline_file_source(
                state,
                workspace_id,
                library_id,
                file_name,
                checksum,
                file_bytes,
            )
            .await?;
        storage_lock.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(storage_key)
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
                    expected_queue_lease_token: None,
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
        let item = admission.items.first().ok_or(ApiError::Internal)?;
        Ok(InlineMutationContext {
            mutation_id: admission.mutation.id,
            job_id: admission.job_id.ok_or(ApiError::Internal)?,
            item_id: item.id,
            workspace_id: admission.mutation.workspace_id,
            library_id: admission.mutation.library_id,
            document_id: item.document_id.ok_or(ApiError::Internal)?,
            revision_id: item.result_revision_id.ok_or(ApiError::Internal)?,
        })
    }

    /// Loads raw source bytes for source-format append.
    pub(super) async fn load_appendable_document_source(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<super::AppendableDocumentSource, ApiError> {
        let head = self.get_document_head(state, document_id).await?;
        let readable_revision_id =
            head.as_ref().and_then(|row| row.readable_revision_id).ok_or_else(|| {
                ApiError::unreadable_document("document has no readable revision".to_string())
            })?;
        let base_revision = content_repository::get_revision_by_id(
            &state.persistence.postgres,
            readable_revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("revision", readable_revision_id))?;
        let storage_key = base_revision.storage_key.as_deref().ok_or_else(|| {
            ApiError::BadRequest(
                "append is not supported for revisions without stored source bytes".to_string(),
            )
        })?;
        if !super::is_appendable_text_mime(&base_revision.mime_type) {
            return Err(ApiError::BadRequest(format!(
                "append is not supported for mime type {} — only text-like sources can be appended",
                base_revision.mime_type
            )));
        }
        // The source blob can be absent even though the content row exists — for
        // example when content rows were restored from a snapshot that did not
        // carry their blobs (`library_data` without `blobs`). Rather than failing
        // the whole append, start a fresh source revision containing only the
        // appended text. A blob that *is* present but cannot be read stays a hard
        // error so a transient storage outage never silently drops existing content.
        let raw_bytes = if state.content_storage.has_revision_source(storage_key).await.map_err(
            |error| ApiError::internal_with_log(error, "failed to probe revision source storage"),
        )? {
            state.content_storage.read_revision_source(storage_key).await.map_err(|error| {
                ApiError::internal_with_log(error, "failed to read revision source from storage")
            })?
        } else {
            tracing::warn!(
                %document_id,
                storage_key,
                "appendable source blob is missing from content storage; starting a fresh source revision",
            );
            Vec::new()
        };
        Ok(super::AppendableDocumentSource {
            raw_bytes,
            mime_type: base_revision.mime_type,
            title: base_revision.title.or_else(|| Some(document_id.to_string())),
            language_code: base_revision.language_code,
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
        if extract
            .normalized_text
            .as_deref()
            .map(str::trim)
            .as_ref()
            .is_none_or(|value| value.is_empty())
        {
            return Err(ApiError::unreadable_document(
                "document is not readable enough for inline text mutations".to_string(),
            ));
        }
        let base_revision = content_repository::get_revision_by_id(
            &state.persistence.postgres,
            readable_revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("revision", readable_revision_id))?;
        Ok(EditableDocumentContext {
            title: base_revision.title.or_else(|| Some(document_id.to_string())),
            language_code: None,
        })
    }

    pub async fn prepare_and_persist_revision_structure(
        &self,
        state: &AppState,
        revision_id: Uuid,
        extraction_plan: &FileExtractionPlan,
        cancellation_token: &CancellationToken,
    ) -> Result<PreparedRevisionPersistenceSummary, ContentServiceError> {
        ensure_not_cancelled(cancellation_token)?;
        let revision =
            content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        if let Some(summary) =
            reusable_prepared_revision_summary(state, &revision, extraction_plan).await?
        {
            return Ok(summary);
        }
        let source_text = extraction_plan.source_text.clone().unwrap_or_default();
        let normalized_text =
            extraction_plan.normalized_text.clone().unwrap_or_else(|| source_text.clone());

        let prepare_structure_start = std::time::Instant::now();
        ensure_not_cancelled(cancellation_token)?;
        // Resolve the library's chunking template so we apply the correct profile.
        let library_row =
            catalog_repository::get_library_by_id(&state.persistence.postgres, revision.library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let chunking_template = library_row.as_ref().map_or(ChunkingTemplate::Naive, |lib| {
            ChunkingTemplate::from_db_str(&lib.chunking_template)
        });
        let preparation_service = StructuredPreparationService::with_template(chunking_template);
        let mut prepared = preparation_service
            .prepare_revision(
                PrepareStructuredRevisionCommand {
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
                },
                cancellation_token,
            )
            .map_err(|error| {
                ApiError::BadRequest(format!(
                    "structured preparation failed for {revision_id}: {error}"
                ))
            })?;
        let prepare_structure_elapsed_ms = prepare_structure_start.elapsed().as_millis() as i64;

        ensure_not_cancelled(cancellation_token)?;
        let extract_technical_facts_start = std::time::Instant::now();
        let extracted_facts = state.canonical_services.technical_facts.extract_from_blocks(
            ExtractTechnicalFactsCommand {
                revision_id,
                document_id: revision.document_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                blocks: prepared.ordered_blocks.clone(),
            },
            cancellation_token,
        )?;
        prepared.prepared_revision.typed_fact_count =
            i32::try_from(extracted_facts.facts.len()).unwrap_or(i32::MAX);
        let extract_technical_facts_elapsed_ms =
            extract_technical_facts_start.elapsed().as_millis() as i64;

        ensure_not_cancelled(cancellation_token)?;
        let chunk_content_start = std::time::Instant::now();
        let now = Utc::now();
        let _ = state
            .document_store
            .upsert_structured_revision(&KnowledgeStructuredRevisionRow {
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
        ensure_not_cancelled(cancellation_token)?;
        let structured_block_rows = prepared
            .ordered_blocks
            .iter()
            .map(|block| KnowledgeStructuredBlockRow {
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
        state
            .document_store
            .replace_structured_blocks(revision_id, &structured_block_rows)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let mut next_search_char = 0usize;
        let mut pending_chunks = Vec::with_capacity(prepared.chunk_windows.len());
        for chunk in &prepared.chunk_windows {
            ensure_not_cancelled(cancellation_token)?;
            let (start_offset, end_offset) =
                locate_chunk_offsets(&normalized_text, &chunk.content_text, next_search_char);
            next_search_char = end_offset;
            // Canonical temporal bounds extraction. JSONL chats stamp every
            // record with `occurred_at=ISO`; the helper aggregates MIN/MAX
            // across all records that landed in this chunk so retrieval
            // can hard-filter by `[t_start, t_end)` without parsing chunk
            // text at query time. Non-temporal sources (PDF/image/markdown)
            // return None and the columns stay NULL.
            let (occurred_at, occurred_until): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
                match crate::shared::extraction::record_jsonl::extract_chunk_temporal_bounds(
                    &chunk.normalized_text,
                ) {
                    Some((min, max)) => (Some(min), Some(max)),
                    None => (None, None),
                };
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
                window_text: chunk.window_text.clone(),
                occurred_at,
                occurred_until,
            });
        }
        let projected_chunks = pending_chunks
            .iter()
            .map(|chunk| content_repository::NewContentChunkProjection {
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                document_id: revision.document_id,
                revision_id,
                chunk_index: chunk.chunk_index,
                start_offset: chunk.start_offset,
                end_offset: chunk.end_offset,
                token_count: chunk.token_count,
                chunk_kind: chunk.chunk_kind.clone(),
                content_text: chunk.content_text.clone(),
                normalized_text: chunk.normalized_text.clone(),
                text_checksum: chunk.text_checksum.clone(),
                support_block_ids: chunk.support_block_ids.clone(),
                section_path: chunk.section_path.clone(),
                heading_trail: chunk.heading_trail.clone(),
                literal_digest: chunk.literal_digest.clone(),
                chunk_state: "ready".to_string(),
                text_generation: Some(i64::from(revision.revision_number)),
                vector_generation: None,
                quality_score: chunk.quality_score,
                window_text: chunk.window_text.clone(),
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
            })
            .collect::<Vec<_>>();
        ensure_not_cancelled(cancellation_token)?;
        // Fence the delete before it becomes visible; chunk replacement bumps
        // the source version again in its own transaction after it succeeds.
        {
            let _vector_guard = state
                .canonical_services
                .search
                .vector_plane_write_guard(state, revision.library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let expected_source_truth_version =
                catalog_repository::get_library_source_truth_version(
                    &state.persistence.postgres,
                    revision.library_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            state
                .search_store
                .delete_chunk_vectors_by_revision_fenced(
                    revision.library_id,
                    revision_id,
                    expected_source_truth_version,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            invalidate_library_embedding_profile_inventory(revision.library_id);
        }
        ensure_not_cancelled(cancellation_token)?;
        let created_chunks = content_repository::replace_chunks_with_projection(
            &state.persistence.postgres,
            revision_id,
            &projected_chunks,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;
        let mut block_to_chunk_ids = std::collections::BTreeMap::<Uuid, Vec<Uuid>>::new();
        for (chunk, pending_chunk) in created_chunks.into_iter().zip(pending_chunks.iter()) {
            ensure_not_cancelled(cancellation_token)?;
            for block_id in &pending_chunk.support_block_ids {
                block_to_chunk_ids.entry(*block_id).or_default().push(chunk.id);
            }
        }

        let mut technical_fact_rows = Vec::with_capacity(extracted_facts.facts.len());
        for fact in &extracted_facts.facts {
            ensure_not_cancelled(cancellation_token)?;
            let support_chunk_ids = fact
                .support_block_ids
                .iter()
                .filter_map(|block_id| block_to_chunk_ids.get(block_id))
                .flatten()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            technical_fact_rows.push(KnowledgeTechnicalFactRow {
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
            });
        }
        let _ = state
            .document_store
            .replace_technical_facts(revision_id, &technical_fact_rows)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;

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

    async fn load_graph_materialization_sources(
        &self,
        state: &AppState,
        command: &MaterializeRevisionGraphCandidatesCommand,
        cancellation_token: &CancellationToken,
    ) -> Result<GraphMaterializationSources, ContentServiceError> {
        ensure_not_cancelled(cancellation_token)?;
        let graph_runtime_context = resolve_runtime_task_execution_context(
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
            .document_store
            .get_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_revision", command.revision_id)
            })?;
        ensure_not_cancelled(cancellation_token)?;
        let document = state
            .document_store
            .get_document(revision.document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_document", revision.document_id)
            })?;
        let canceled_orphans =
            repositories::cancel_processing_runtime_graph_extractions_for_document(
                &state.persistence.postgres,
                command.library_id,
                document.document_id,
                "graph extraction restarted before this chunk completed",
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        log_canceled_graph_orphans(command, &document, canceled_orphans);
        ensure_not_cancelled(cancellation_token)?;
        let all_chunks = state
            .document_store
            .list_chunks_by_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;
        let structured_revision = state
            .document_store
            .get_structured_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;
        let revision_facts = state
            .canonical_services
            .knowledge
            .list_typed_technical_facts(state, command.revision_id)
            .await?;
        ensure_not_cancelled(cancellation_token)?;
        let structured_blocks = state
            .document_store
            .list_structured_blocks_by_revision(command.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;
        let table_graph_context = build_revision_table_graph_context(&structured_blocks);
        let library_extraction_prompt =
            catalog_repository::get_library_by_id(&state.persistence.postgres, command.library_id)
                .await
                .ok()
                .flatten()
                .and_then(|row| row.extraction_prompt);
        let sub_type_hints = load_sub_type_hints_for_extraction(state, command.library_id).await;
        Ok(GraphMaterializationSources {
            graph_runtime_context,
            revision,
            document,
            all_chunks,
            structured_revision,
            revision_facts,
            table_graph_context,
            library_extraction_prompt,
            sub_type_hints,
        })
    }

    pub async fn materialize_revision_graph_candidates(
        &self,
        state: &AppState,
        command: MaterializeRevisionGraphCandidatesCommand,
        cancellation_token: &CancellationToken,
    ) -> Result<RevisionGraphCandidateMaterialization, ContentServiceError> {
        let sources =
            self.load_graph_materialization_sources(state, &command, cancellation_token).await?;
        let selection = graph_materialization_selection(&sources);
        log_graph_materialization_selection(&command, &selection);
        clear_revision_graph_candidates(state, command.revision_id, cancellation_token).await?;
        let binding = resolve_graph_materialization_binding(state, command.library_id).await?;
        let cache = self
            .resolve_graph_materialization_cache(
                state,
                &command,
                &sources,
                &selection,
                &binding,
                cancellation_token,
            )
            .await?;
        log_graph_materialization_cache(&command, &selection, &cache);
        let extraction = prepare_graph_chunk_extraction(selection, sources.all_chunks, cache);
        sync_extract_graph_stage_progress(
            state,
            command.attempt_id,
            extraction.result_seed.reused_chunk_count,
            extraction.progress_total,
        )
        .await;
        let PreparedGraphChunkExtraction {
            result_seed,
            chunks,
            graph_chunk_policy,
            progress_total,
        } = extraction;
        let aggregate = extract_remaining_graph_chunks(GraphChunkExtractionBatch {
            state,
            command: &command,
            graph_runtime_context: sources.graph_runtime_context,
            document: sources.document,
            revision: sources.revision,
            revision_facts: sources.revision_facts,
            library_extraction_prompt: sources.library_extraction_prompt,
            sub_type_hints: sources.sub_type_hints,
            table_graph_context: sources.table_graph_context,
            graph_chunk_policy,
            chunks,
            progress_total,
            progress_completed: result_seed.reused_chunk_count,
            cancellation_token,
        })
        .await?;
        Ok(graph_materialization_result(result_seed, aggregate, binding))
    }

    async fn resolve_graph_materialization_cache(
        &self,
        state: &AppState,
        command: &MaterializeRevisionGraphCandidatesCommand,
        sources: &GraphMaterializationSources,
        selection: &GraphMaterializationSelection,
        binding: &GraphMaterializationBinding,
        cancellation_token: &CancellationToken,
    ) -> anyhow::Result<GraphExtractionCachePlan> {
        self.materialize_cached_graph_extractions(CachedGraphExtractionMaterialization {
            state,
            command,
            document: &sources.document,
            revision: &sources.revision,
            chunks: sources.all_chunks.as_slice(),
            revision_facts: sources.revision_facts.as_slice(),
            library_extraction_prompt: &sources.library_extraction_prompt,
            sub_type_hints: &sources.sub_type_hints,
            table_graph_context: &sources.table_graph_context,
            graph_chunk_policy: &selection.graph_chunk_policy,
            provider_kind: &binding.provider_kind,
            model_name: &binding.model_name,
            runtime_binding: &binding.runtime_binding,
            request_size_soft_limit_bytes: binding.request_size_soft_limit_bytes,
            cancellation_token,
        })
        .await
    }

    async fn reuse_cached_graph_extraction_for_chunk(
        &self,
        materialization: &CachedGraphExtractionMaterialization<'_>,
        chunk: &KnowledgeChunkRow,
    ) -> anyhow::Result<Option<(usize, usize, bool)>> {
        let CachedGraphExtractionMaterialization {
            state,
            command,
            document,
            revision,
            revision_facts,
            library_extraction_prompt,
            sub_type_hints,
            table_graph_context,
            graph_chunk_policy,
            provider_kind,
            model_name,
            runtime_binding,
            request_size_soft_limit_bytes,
            cancellation_token,
            ..
        } = materialization;
        ensure_not_cancelled(cancellation_token)?;
        let Some(chunk_content) = build_graph_chunk_content(
            chunk,
            table_graph_context.profile_for_chunk(chunk),
            table_graph_context.requires_row_only_graph(),
            graph_chunk_policy,
        ) else {
            return Ok(None);
        };
        let chunk_facts = revision_facts
            .iter()
            .filter(|fact| typed_fact_supports_chunk(fact, chunk))
            .cloned()
            .collect::<Vec<_>>();
        let request = build_canonical_graph_extraction_request(
            document,
            revision,
            chunk,
            chunk_content,
            &chunk_facts,
            command.attempt_id,
            (*library_extraction_prompt).clone(),
            (*sub_type_hints).clone(),
        );
        let fingerprint = build_graph_extraction_cache_fingerprint(
            &request,
            runtime_binding,
            *request_size_soft_limit_bytes,
        );
        let text_checksum = sha256_hex_text(&chunk.normalized_text);
        let Some(record) =
            crate::infra::repositories::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
                &state.persistence.postgres,
                command.library_id,
                &text_checksum,
                &fingerprint.extraction_version,
                provider_kind,
                model_name,
                &fingerprint.prompt_hash,
            )
            .await
            .map_err(|e| {
                ApiError::internal_with_log(
                    e,
                    "graph_cache: find_ready_runtime_graph_extraction_record_by_semantic_cache_key",
                )
            })?
        else {
            return Ok(None);
        };
        let prompt_hash_mismatch = record.prompt_hash != fingerprint.prompt_hash;
        let original_counts = graph_candidate_counts(&record.normalized_output_json);
        let repaired_candidate_set = serde_json::from_value(record.normalized_output_json.clone())
            .map(repair_graph_extraction_candidate_set)
            .unwrap_or_default();
        if original_counts.0.saturating_add(original_counts.1) > 0
            && repaired_candidate_set.entities.is_empty()
            && repaired_candidate_set.relations.is_empty()
        {
            return Ok(None);
        }
        let repaired_json = canonical_graph_extraction_normalized_json(repaired_candidate_set);
        if record.chunk_id == chunk.chunk_id {
            let lifecycle = extraction_lifecycle_from_record(&record);
            if lifecycle.revision_id.is_some() && lifecycle.revision_id != Some(command.revision_id)
            {
                return Ok(None);
            }
        }
        ensure_not_cancelled(cancellation_token)?;
        persist_reused_graph_extraction(
            state,
            command,
            revision,
            chunk,
            &record,
            &repaired_json,
            cancellation_token,
        )
        .await?;
        let (entity_count, relation_count) = graph_candidate_counts(&repaired_json);
        Ok(Some((entity_count, relation_count, prompt_hash_mismatch)))
    }

    async fn materialize_cached_graph_extractions(
        &self,
        materialization: CachedGraphExtractionMaterialization<'_>,
    ) -> anyhow::Result<GraphExtractionCachePlan> {
        let mut plan = GraphExtractionCachePlan::default();
        for chunk in materialization.chunks {
            let Some((entity_count, relation_count, prompt_hash_mismatch)) =
                self.reuse_cached_graph_extraction_for_chunk(&materialization, chunk).await?
            else {
                continue;
            };
            plan.reused_prompt_hash_mismatch_count = plan
                .reused_prompt_hash_mismatch_count
                .saturating_add(usize::from(prompt_hash_mismatch));
            plan.reused_entity_count = plan.reused_entity_count.saturating_add(entity_count);
            plan.reused_relation_count = plan.reused_relation_count.saturating_add(relation_count);
            plan.reused_chunk_ids.insert(chunk.chunk_id);
        }
        Ok(plan)
    }
}

struct GraphMaterializationSelection {
    chunk_count: usize,
    selected_graph_chunk_count: usize,
    record_stream_source_units_skipped: usize,
    graph_chunk_policy: GraphExtractionChunkPolicy,
}

struct GraphMaterializationBinding {
    runtime_binding: ResolvedRuntimeBinding,
    provider_kind: String,
    model_name: String,
    request_size_soft_limit_bytes: usize,
}

struct GraphMaterializationResultSeed {
    chunk_count: usize,
    selected_graph_chunk_count: usize,
    record_stream_source_units_skipped: usize,
    reused_chunk_count: usize,
    reused_prompt_hash_mismatch_count: usize,
    reused_entity_count: usize,
    reused_relation_count: usize,
}

struct PreparedGraphChunkExtraction {
    result_seed: GraphMaterializationResultSeed,
    chunks: Vec<KnowledgeChunkRow>,
    graph_chunk_policy: GraphExtractionChunkPolicy,
    progress_total: usize,
}

struct GraphChunkExtractionBatch<'a> {
    state: &'a AppState,
    command: &'a MaterializeRevisionGraphCandidatesCommand,
    graph_runtime_context: crate::services::ingest::runtime::RuntimeTaskExecutionContext,
    document: KnowledgeDocumentRow,
    revision: KnowledgeRevisionRow,
    revision_facts: Vec<TypedTechnicalFact>,
    library_extraction_prompt: Option<String>,
    sub_type_hints: GraphExtractionSubTypeHints,
    table_graph_context: RevisionTableGraphContext,
    graph_chunk_policy: GraphExtractionChunkPolicy,
    chunks: Vec<KnowledgeChunkRow>,
    progress_total: usize,
    progress_completed: usize,
    cancellation_token: &'a CancellationToken,
}

fn graph_materialization_selection(
    sources: &GraphMaterializationSources,
) -> GraphMaterializationSelection {
    let graph_chunk_policy = build_graph_extraction_chunk_policy(
        sources.structured_revision.as_ref(),
        sources.all_chunks.as_slice(),
        sources.revision_facts.as_slice(),
    );
    let selected_graph_chunk_count = selected_graph_chunk_count(
        &sources.all_chunks,
        &sources.table_graph_context,
        &graph_chunk_policy,
    );
    GraphMaterializationSelection {
        chunk_count: sources.all_chunks.len(),
        selected_graph_chunk_count,
        record_stream_source_units_skipped: record_stream_source_units_skipped(
            &sources.all_chunks,
            &graph_chunk_policy,
        ),
        graph_chunk_policy,
    }
}

fn selected_graph_chunk_count(
    chunks: &[KnowledgeChunkRow],
    table_graph_context: &RevisionTableGraphContext,
    graph_chunk_policy: &GraphExtractionChunkPolicy,
) -> usize {
    chunks
        .iter()
        .filter(|chunk| {
            build_graph_chunk_content(
                chunk,
                table_graph_context.profile_for_chunk(chunk),
                table_graph_context.requires_row_only_graph(),
                graph_chunk_policy,
            )
            .is_some()
        })
        .count()
}

fn log_graph_materialization_selection(
    command: &MaterializeRevisionGraphCandidatesCommand,
    selection: &GraphMaterializationSelection,
) {
    if !selection.graph_chunk_policy.is_record_stream() {
        return;
    }
    tracing::info!(
        revision_id = %command.revision_id,
        selected_graph_chunks = selection.selected_graph_chunk_count,
        selected_source_units = selection.graph_chunk_policy.selected_source_unit_count(),
        skipped_source_units = selection.record_stream_source_units_skipped,
        "record-stream graph extraction chunk policy selected representative source units",
    );
}

async fn clear_revision_graph_candidates(
    state: &AppState,
    revision_id: Uuid,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<()> {
    let _ = state
        .graph_store
        .delete_entity_candidates_by_revision(revision_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    ensure_not_cancelled(cancellation_token)?;
    let _ = state
        .graph_store
        .delete_relation_candidates_by_revision(revision_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(())
}

async fn resolve_graph_materialization_binding(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<GraphMaterializationBinding> {
    let runtime_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractGraph)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest(
                "active extract_graph binding is required for graph extraction".to_string(),
            )
        })?;
    Ok(GraphMaterializationBinding {
        provider_kind: runtime_binding.provider_kind.clone(),
        model_name: runtime_binding.model_name.clone(),
        request_size_soft_limit_bytes: state
            .resolve_settle_blockers_services
            .provider_failure_classification
            .request_size_soft_limit_bytes(),
        runtime_binding,
    })
}

fn log_graph_materialization_cache(
    command: &MaterializeRevisionGraphCandidatesCommand,
    selection: &GraphMaterializationSelection,
    cache: &GraphExtractionCachePlan,
) {
    if cache.reused_chunk_ids.is_empty() {
        return;
    }
    tracing::info!(
        revision_id = %command.revision_id,
        total_chunks = selection.chunk_count,
        selected_graph_chunks = selection.selected_graph_chunk_count,
        reused = cache.reused_chunk_ids.len(),
        reused_prompt_hash_mismatches = cache.reused_prompt_hash_mismatch_count,
        reused_entities = cache.reused_entity_count,
        reused_relations = cache.reused_relation_count,
        "graph extraction cache: reusing ready extraction output",
    );
}

fn prepare_graph_chunk_extraction(
    selection: GraphMaterializationSelection,
    chunks: Vec<KnowledgeChunkRow>,
    cache: GraphExtractionCachePlan,
) -> PreparedGraphChunkExtraction {
    let remaining_chunks = chunks
        .into_iter()
        .filter(|chunk| !cache.reused_chunk_ids.contains(&chunk.chunk_id))
        .collect::<Vec<_>>();
    let reused_chunk_count = cache.reused_chunk_ids.len();
    let remaining_graph_chunk_count =
        selection.selected_graph_chunk_count.saturating_sub(reused_chunk_count);
    PreparedGraphChunkExtraction {
        result_seed: GraphMaterializationResultSeed {
            chunk_count: selection.chunk_count,
            selected_graph_chunk_count: selection.selected_graph_chunk_count,
            record_stream_source_units_skipped: selection.record_stream_source_units_skipped,
            reused_chunk_count,
            reused_prompt_hash_mismatch_count: cache.reused_prompt_hash_mismatch_count,
            reused_entity_count: cache.reused_entity_count,
            reused_relation_count: cache.reused_relation_count,
        },
        chunks: remaining_chunks,
        graph_chunk_policy: selection.graph_chunk_policy,
        progress_total: reused_chunk_count.saturating_add(remaining_graph_chunk_count),
    }
}

async fn extract_remaining_graph_chunks(
    batch: GraphChunkExtractionBatch<'_>,
) -> anyhow::Result<ChunkExtractAggregate> {
    let GraphChunkExtractionBatch {
        state,
        command,
        graph_runtime_context,
        document,
        revision,
        revision_facts,
        library_extraction_prompt,
        sub_type_hints,
        table_graph_context,
        graph_chunk_policy,
        chunks,
        progress_total,
        progress_completed,
        cancellation_token,
    } = batch;
    let total_to_extract = chunks.len();
    let graph_extract_parallelism =
        state.settings.ingestion_graph_extract_parallelism_per_doc.max(1);
    let document = std::sync::Arc::new(document);
    let revision = std::sync::Arc::new(revision);
    let revision_facts = std::sync::Arc::new(revision_facts);
    let library_extraction_prompt = std::sync::Arc::new(library_extraction_prompt);
    let sub_type_hints = std::sync::Arc::new(sub_type_hints);
    let table_graph_context = std::sync::Arc::new(table_graph_context);
    let graph_chunk_policy = std::sync::Arc::new(graph_chunk_policy);
    let per_chunk_stream = stream::iter(chunks.into_iter().map(|chunk| {
        let task = GraphChunkExtractionTask {
            state: state.clone(),
            command: command.clone(),
            graph_runtime_context: graph_runtime_context.clone(),
            document: std::sync::Arc::clone(&document),
            revision: std::sync::Arc::clone(&revision),
            revision_facts: std::sync::Arc::clone(&revision_facts),
            library_extraction_prompt: std::sync::Arc::clone(&library_extraction_prompt),
            sub_type_hints: std::sync::Arc::clone(&sub_type_hints),
            table_graph_context: std::sync::Arc::clone(&table_graph_context),
            graph_chunk_policy: std::sync::Arc::clone(&graph_chunk_policy),
            cancellation_token: cancellation_token.clone(),
        };
        async move { extract_graph_chunk(task, chunk).await }
    }));
    let progress_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let progress_reporter = spawn_graph_progress_reporter(
        std::sync::Arc::clone(&progress_counter),
        command.library_id,
        command.revision_id,
        total_to_extract,
    );
    let idle_timeout = std::time::Duration::from_secs(
        state.settings.runtime_graph_extract_idle_timeout_seconds.max(1),
    );
    let result = fold_graph_chunk_extractions(GraphChunkFold {
        state,
        command,
        per_chunk_stream,
        graph_extract_parallelism,
        idle_timeout,
        progress_counter,
        progress_total,
        progress_completed,
        cancellation_token,
    })
    .await;
    progress_reporter.abort();
    result
}

struct GraphChunkExtractionTask {
    state: AppState,
    command: MaterializeRevisionGraphCandidatesCommand,
    graph_runtime_context: crate::services::ingest::runtime::RuntimeTaskExecutionContext,
    document: std::sync::Arc<KnowledgeDocumentRow>,
    revision: std::sync::Arc<KnowledgeRevisionRow>,
    revision_facts: std::sync::Arc<Vec<TypedTechnicalFact>>,
    library_extraction_prompt: std::sync::Arc<Option<String>>,
    sub_type_hints: std::sync::Arc<GraphExtractionSubTypeHints>,
    table_graph_context: std::sync::Arc<RevisionTableGraphContext>,
    graph_chunk_policy: std::sync::Arc<GraphExtractionChunkPolicy>,
    cancellation_token: CancellationToken,
}

async fn extract_graph_chunk(
    task: GraphChunkExtractionTask,
    chunk: KnowledgeChunkRow,
) -> anyhow::Result<ChunkExtractAggregate> {
    ensure_not_cancelled(&task.cancellation_token)?;
    let Some(chunk_content) = build_graph_chunk_content(
        &chunk,
        task.table_graph_context.profile_for_chunk(&chunk),
        task.table_graph_context.requires_row_only_graph(),
        &task.graph_chunk_policy,
    ) else {
        return Ok(ChunkExtractAggregate::default());
    };
    let chunk_facts = task
        .revision_facts
        .iter()
        .filter(|fact| typed_fact_supports_chunk(fact, &chunk))
        .cloned()
        .collect::<Vec<_>>();
    ensure_not_cancelled(&task.cancellation_token)?;
    let response = extract_chunk_graph_candidates(
        &task.state,
        &task.graph_runtime_context,
        &build_canonical_graph_extraction_request(
            &task.document,
            &task.revision,
            &chunk,
            chunk_content,
            &chunk_facts,
            task.command.attempt_id,
            (*task.library_extraction_prompt).clone(),
            (*task.sub_type_hints).clone(),
        ),
        &task.cancellation_token,
    )
    .await
    .map_err(|error| graph_chunk_extraction_error(chunk.chunk_id, error))?;
    ensure_not_cancelled(&task.cancellation_token)?;
    capture_graph_chunk_billing(&task, &chunk, &response).await?;
    Ok(graph_chunk_aggregate(&response))
}

fn graph_chunk_extraction_error(
    chunk_id: Uuid,
    error: GraphExtractionExecutionError,
) -> anyhow::Error {
    if error.cancelled {
        return anyhow::Error::new(crate::services::ingest::cancellation::StageError::Cancelled);
    }
    anyhow::anyhow!("graph extraction failed for chunk {chunk_id}: {}", error.message)
}

async fn capture_graph_chunk_billing(
    task: &GraphChunkExtractionTask,
    chunk: &KnowledgeChunkRow,
    response: &GraphExtractionOutcome,
) -> anyhow::Result<()> {
    let graph_extraction_id = response.graph_extraction_id.ok_or_else(|| {
        anyhow::anyhow!("graph extraction response is missing canonical graph_extraction_id")
    })?;
    let runtime_execution_id = response.runtime_execution_id.ok_or_else(|| {
        anyhow::anyhow!("graph extraction response is missing canonical runtime_execution_id")
    })?;
    if let Err(error) = task
        .state
        .canonical_services
        .billing
        .capture_graph_extraction(
            &task.state,
            CaptureGraphExtractionBillingCommand {
                workspace_id: task.command.workspace_id,
                library_id: task.command.library_id,
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
            revision_id = %task.command.revision_id,
            chunk_id = %chunk.chunk_id,
            graph_extraction_id = %graph_extraction_id,
            runtime_execution_id = %runtime_execution_id,
            ?error,
            "graph extraction billing capture failed; continuing canonical graph admission",
        );
    }
    Ok(())
}

fn graph_chunk_aggregate(response: &GraphExtractionOutcome) -> ChunkExtractAggregate {
    ChunkExtractAggregate {
        processed_graph_chunks: 1,
        extracted_entities: response.normalized.entities.len(),
        extracted_relations: response.normalized.relations.len(),
        prompt_tokens: usage_token_count(&response.usage_json, "prompt_tokens"),
        completion_tokens: usage_token_count(&response.usage_json, "completion_tokens"),
        total_tokens: usage_token_count(&response.usage_json, "total_tokens"),
    }
}

fn usage_token_count(usage_json: &serde_json::Value, field: &str) -> i64 {
    usage_json.get(field).and_then(serde_json::Value::as_i64).unwrap_or(0)
}

fn spawn_graph_progress_reporter(
    counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
    library_id: Uuid,
    revision_id: Uuid,
    total: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let done = counter.load(std::sync::atomic::Ordering::Relaxed) as usize;
            if done >= total {
                break;
            }
            tracing::info!(
                library_id = %library_id,
                revision_id = %revision_id,
                progress_pct = done * 100 / total.max(1),
                done,
                total,
                "extract_graph: chunk progress"
            );
        }
    })
}

struct GraphChunkFold<'a, S> {
    state: &'a AppState,
    command: &'a MaterializeRevisionGraphCandidatesCommand,
    per_chunk_stream: S,
    graph_extract_parallelism: usize,
    idle_timeout: std::time::Duration,
    progress_counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
    progress_total: usize,
    progress_completed: usize,
    cancellation_token: &'a CancellationToken,
}

async fn fold_graph_chunk_extractions<S, F>(
    fold: GraphChunkFold<'_, S>,
) -> anyhow::Result<ChunkExtractAggregate>
where
    S: futures::Stream<Item = F>,
    F: std::future::Future<Output = anyhow::Result<ChunkExtractAggregate>>,
{
    let buffered = fold.per_chunk_stream.buffer_unordered(fold.graph_extract_parallelism);
    futures::pin_mut!(buffered);
    let mut aggregate = ChunkExtractAggregate::default();
    let mut progress_completed = fold.progress_completed;
    loop {
        let item = match tokio::time::timeout(fold.idle_timeout, buffered.try_next()).await {
            Ok(Ok(Some(item))) => item,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                return Err(graph_extraction_idle_timeout(
                    fold.command.revision_id,
                    fold.idle_timeout,
                ));
            }
        };
        ensure_not_cancelled(fold.cancellation_token)?;
        aggregate.add(&item);
        progress_completed = sync_graph_chunk_progress(
            fold.state,
            fold.command,
            progress_completed,
            fold.progress_total,
            item.processed_graph_chunks,
        )
        .await;
        fold.progress_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(aggregate)
}

fn graph_extraction_idle_timeout(
    revision_id: Uuid,
    idle_timeout: std::time::Duration,
) -> anyhow::Error {
    anyhow::anyhow!(
        "graph extraction idle timeout: no chunk completed for revision {} within {}s",
        revision_id,
        idle_timeout.as_secs()
    )
}

async fn sync_graph_chunk_progress(
    state: &AppState,
    command: &MaterializeRevisionGraphCandidatesCommand,
    progress_completed: usize,
    progress_total: usize,
    processed_graph_chunks: usize,
) -> usize {
    if processed_graph_chunks == 0 {
        return progress_completed;
    }
    let completed = progress_completed.saturating_add(processed_graph_chunks);
    if should_sync_stage_unit_progress(completed, progress_total) {
        sync_extract_graph_stage_progress(state, command.attempt_id, completed, progress_total)
            .await;
    }
    completed
}

fn graph_materialization_result(
    result_seed: GraphMaterializationResultSeed,
    aggregate: ChunkExtractAggregate,
    binding: GraphMaterializationBinding,
) -> RevisionGraphCandidateMaterialization {
    RevisionGraphCandidateMaterialization {
        chunk_count: result_seed.chunk_count,
        selected_graph_chunks: result_seed.selected_graph_chunk_count,
        extracted_entities: aggregate
            .extracted_entities
            .saturating_add(result_seed.reused_entity_count),
        extracted_relations: aggregate
            .extracted_relations
            .saturating_add(result_seed.reused_relation_count),
        provider_kind: Some(binding.provider_kind),
        model_name: Some(binding.model_name),
        usage_json: serde_json::json!({
            "prompt_tokens": aggregate.prompt_tokens,
            "completion_tokens": aggregate.completion_tokens,
            "total_tokens": aggregate.total_tokens,
        }),
        reused_chunks: result_seed.reused_chunk_count,
        reused_prompt_hash_mismatches: result_seed.reused_prompt_hash_mismatch_count,
        reused_entities: result_seed.reused_entity_count,
        reused_relations: result_seed.reused_relation_count,
        record_stream_source_units_skipped: result_seed.record_stream_source_units_skipped,
    }
}

struct GraphMaterializationSources {
    graph_runtime_context: crate::services::ingest::runtime::RuntimeTaskExecutionContext,
    revision: KnowledgeRevisionRow,
    document: KnowledgeDocumentRow,
    all_chunks: Vec<KnowledgeChunkRow>,
    structured_revision: Option<KnowledgeStructuredRevisionRow>,
    revision_facts: Vec<TypedTechnicalFact>,
    table_graph_context: RevisionTableGraphContext,
    library_extraction_prompt: Option<String>,
    sub_type_hints: GraphExtractionSubTypeHints,
}

fn log_canceled_graph_orphans(
    command: &MaterializeRevisionGraphCandidatesCommand,
    document: &KnowledgeDocumentRow,
    canceled_orphans: u64,
) {
    if canceled_orphans == 0 {
        return;
    }
    info!(
        library_id = %command.library_id,
        revision_id = %command.revision_id,
        document_id = %document.document_id,
        canceled_orphans,
        "graph extraction resume canceled orphaned processing chunk rows"
    );
}

async fn persist_reused_graph_extraction(
    state: &AppState,
    command: &MaterializeRevisionGraphCandidatesCommand,
    revision: &KnowledgeRevisionRow,
    chunk: &KnowledgeChunkRow,
    record: &crate::infra::repositories::RuntimeGraphExtractionRecordRow,
    repaired_normalized_output_json: &serde_json::Value,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<()> {
    if record.chunk_id == chunk.chunk_id {
        return update_same_chunk_cached_graph_extraction(
            state,
            record,
            repaired_normalized_output_json,
            cancellation_token,
        )
        .await;
    }

    let raw_output_json =
        graph_cache_reuse_raw_output(record, command.revision_id, command.attempt_id);
    crate::infra::repositories::create_runtime_graph_extraction_record(
        &state.persistence.postgres,
        &crate::infra::repositories::CreateRuntimeGraphExtractionRecordInput {
            id: Uuid::now_v7(),
            runtime_execution_id: record.runtime_execution_id,
            library_id: command.library_id,
            document_id: revision.document_id,
            chunk_id: chunk.chunk_id,
            provider_kind: record.provider_kind.clone(),
            model_name: record.model_name.clone(),
            extraction_version: record.extraction_version.clone(),
            prompt_hash: record.prompt_hash.clone(),
            status: ASYNC_OP_STATUS_READY.to_string(),
            raw_output_json,
            normalized_output_json: repaired_normalized_output_json.clone(),
            glean_pass_count: 0,
            error_message: None,
        },
    )
    .await
    .map_err(|e| {
        ApiError::internal_with_log(e, "graph_cache: create_runtime_graph_extraction_record")
    })?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(())
}

async fn update_same_chunk_cached_graph_extraction(
    state: &AppState,
    record: &crate::infra::repositories::RuntimeGraphExtractionRecordRow,
    repaired_normalized_output_json: &serde_json::Value,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<()> {
    if repaired_normalized_output_json == &record.normalized_output_json {
        return Ok(());
    }
    crate::infra::repositories::update_runtime_graph_extraction_record_safe(
        &state.persistence.postgres,
        record.id,
        &crate::infra::repositories::UpdateRuntimeGraphExtractionRecordInput {
            provider_kind: record.provider_kind.clone(),
            model_name: record.model_name.clone(),
            prompt_hash: record.prompt_hash.clone(),
            status: record.status.clone(),
            raw_output_json: record.raw_output_json.clone(),
            normalized_output_json: repaired_normalized_output_json.clone(),
            glean_pass_count: record.glean_pass_count,
            error_message: record.error_message.clone(),
        },
    )
    .await
    .map_err(|e| {
        ApiError::internal_with_log(e, "graph_cache: update_runtime_graph_extraction_record_safe")
    })?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(())
}

struct CachedGraphExtractionMaterialization<'a> {
    state: &'a AppState,
    command: &'a MaterializeRevisionGraphCandidatesCommand,
    document: &'a KnowledgeDocumentRow,
    revision: &'a KnowledgeRevisionRow,
    chunks: &'a [KnowledgeChunkRow],
    revision_facts: &'a [TypedTechnicalFact],
    library_extraction_prompt: &'a Option<String>,
    sub_type_hints: &'a GraphExtractionSubTypeHints,
    table_graph_context: &'a RevisionTableGraphContext,
    graph_chunk_policy: &'a GraphExtractionChunkPolicy,
    provider_kind: &'a str,
    model_name: &'a str,
    runtime_binding: &'a ResolvedRuntimeBinding,
    request_size_soft_limit_bytes: usize,
    cancellation_token: &'a CancellationToken,
}

#[derive(Debug, Default)]
struct GraphExtractionCachePlan {
    reused_chunk_ids: BTreeSet<Uuid>,
    reused_prompt_hash_mismatch_count: usize,
    reused_entity_count: usize,
    reused_relation_count: usize,
}

fn graph_cache_reuse_raw_output(
    record: &crate::infra::repositories::RuntimeGraphExtractionRecordRow,
    revision_id: Uuid,
    activated_by_attempt_id: Option<Uuid>,
) -> serde_json::Value {
    let mut raw_output_json = record.raw_output_json.clone();
    if let Some(obj) = raw_output_json.as_object_mut() {
        obj.insert(
            "lifecycle".to_string(),
            serde_json::json!({
                "revision_id": revision_id,
                "activated_by_attempt_id": activated_by_attempt_id,
            }),
        );
        obj.insert(
            "reuse".to_string(),
            serde_json::json!({
                "source": "graph_extraction_cache",
                "source_extraction_id": record.id,
                "source_chunk_id": record.chunk_id,
            }),
        );
    }
    raw_output_json
}

fn graph_candidate_counts(normalized_output_json: &serde_json::Value) -> (usize, usize) {
    let entity_count = normalized_output_json
        .get("entities")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let relation_count = normalized_output_json
        .get("relations")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    (entity_count, relation_count)
}

fn build_graph_extraction_chunk_policy(
    structured_revision: Option<&KnowledgeStructuredRevisionRow>,
    chunks: &[KnowledgeChunkRow],
    revision_facts: &[TypedTechnicalFact],
) -> GraphExtractionChunkPolicy {
    if !revision_is_record_stream(structured_revision) {
        return GraphExtractionChunkPolicy::standard();
    }
    GraphExtractionChunkPolicy::record_stream(select_record_stream_source_units(
        chunks,
        revision_facts,
    ))
}

fn revision_is_record_stream(structured_revision: Option<&KnowledgeStructuredRevisionRow>) -> bool {
    structured_revision.is_some_and(|revision| revision.source_format == RECORD_JSONL_SOURCE_FORMAT)
}

fn select_record_stream_source_units(
    chunks: &[KnowledgeChunkRow],
    revision_facts: &[TypedTechnicalFact],
) -> BTreeSet<Uuid> {
    let mut source_units = chunks
        .iter()
        .filter(|chunk| chunk.chunk_kind.as_deref() == Some("source_unit"))
        .collect::<Vec<_>>();
    source_units.sort_by_key(|chunk| chunk.chunk_index);
    if source_units.is_empty() {
        return BTreeSet::new();
    }

    let mut selected_indices = BTreeSet::<usize>::new();
    selected_indices.insert(0);
    selected_indices.insert(source_units.len() - 1);

    for (index, chunk) in source_units.iter().enumerate() {
        if selected_indices.len() >= RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT {
            break;
        }
        if revision_facts.iter().any(|fact| typed_fact_supports_chunk(fact, chunk)) {
            selected_indices.insert(index);
        }
    }

    for index in evenly_spaced_source_unit_indices(
        source_units.len(),
        RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT,
    ) {
        if selected_indices.len() >= RECORD_STREAM_REPRESENTATIVE_SOURCE_UNIT_LIMIT {
            break;
        }
        selected_indices.insert(index);
    }

    selected_indices
        .into_iter()
        .filter_map(|index| source_units.get(index).map(|chunk| chunk.chunk_id))
        .collect()
}

fn evenly_spaced_source_unit_indices(source_unit_count: usize, limit: usize) -> Vec<usize> {
    if source_unit_count == 0 || limit == 0 {
        return Vec::new();
    }
    if source_unit_count <= limit {
        return (0..source_unit_count).collect();
    }
    if limit == 1 {
        return vec![0];
    }
    (0..limit).map(|slot| slot * (source_unit_count - 1) / (limit - 1)).collect()
}

fn record_stream_source_units_skipped(
    chunks: &[KnowledgeChunkRow],
    policy: &GraphExtractionChunkPolicy,
) -> usize {
    if !policy.is_record_stream() {
        return 0;
    }
    chunks
        .iter()
        .filter(|chunk| chunk.chunk_kind.as_deref() == Some("source_unit"))
        .count()
        .saturating_sub(policy.selected_source_unit_count())
}

#[cfg(test)]
fn test_graph_input_hashes_by_chunk(
    chunks: &[KnowledgeChunkRow],
    table_graph_context: &RevisionTableGraphContext,
) -> HashMap<Uuid, String> {
    let policy = GraphExtractionChunkPolicy::standard();
    chunks
        .iter()
        .filter_map(|chunk| {
            let graph_input = build_graph_chunk_content(
                chunk,
                table_graph_context.profile_for_chunk(chunk),
                table_graph_context.requires_row_only_graph(),
                &policy,
            )?;
            Some((chunk.chunk_id, sha256_hex_text(&graph_input)))
        })
        .collect()
}

/// Per-chunk graph extraction outcome: only the small fields the caller
/// aggregates, keeping ~96 bytes per chunk in flight.
#[derive(Debug, Default)]
struct ChunkExtractAggregate {
    processed_graph_chunks: usize,
    extracted_entities: usize,
    extracted_relations: usize,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

impl ChunkExtractAggregate {
    fn add(&mut self, item: &Self) {
        self.processed_graph_chunks =
            self.processed_graph_chunks.saturating_add(item.processed_graph_chunks);
        self.extracted_entities = self.extracted_entities.saturating_add(item.extracted_entities);
        self.extracted_relations =
            self.extracted_relations.saturating_add(item.extracted_relations);
        self.prompt_tokens += item.prompt_tokens;
        self.completion_tokens += item.completion_tokens;
        self.total_tokens += item.total_tokens;
    }
}

#[derive(Clone, Default)]
struct RevisionTableGraphContext {
    by_row_block_id: HashMap<Uuid, TableGraphProfile>,
    row_only_table_graph: bool,
}

impl RevisionTableGraphContext {
    fn profile_for_chunk(
        &self,
        chunk: &crate::infra::knowledge_rows::KnowledgeChunkRow,
    ) -> Option<&TableGraphProfile> {
        chunk.support_block_ids.iter().find_map(|block_id| self.by_row_block_id.get(block_id))
    }

    const fn requires_row_only_graph(&self) -> bool {
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
/// (`node_type`, `sub_type`) pairs out of thousands — so a short TTL is
/// sound; readers see at worst a minute-old hint set.
///
/// Cache is keyed by `(library_id, projection_version)` so a new
/// projection version (published at the end of a full graph rebuild)
/// transparently invalidates. Missing/stale entries fall through to
/// the SQL path; SQL failure still yields empty hints, matching the
/// prior fail-open behaviour.
const SUB_TYPE_HINTS_CACHE_TTL: std::time::Duration = std::time::Duration::from_mins(1);
const SUB_TYPE_HINTS_CACHE_CAPACITY: usize = 64;

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
        let guard =
            sub_type_hints_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get(&(library_id, projection_scope.projection_version))
            && entry.fetched_at.elapsed() < SUB_TYPE_HINTS_CACHE_TTL
        {
            return entry.hints.clone();
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
        let mut guard =
            sub_type_hints_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.retain(|_, entry| entry.fetched_at.elapsed() < SUB_TYPE_HINTS_CACHE_TTL);
        while guard.len() >= SUB_TYPE_HINTS_CACHE_CAPACITY {
            let Some(oldest) =
                guard.iter().min_by_key(|(_, entry)| entry.fetched_at).map(|(key, _)| *key)
            else {
                break;
            };
            guard.remove(&oldest);
        }
        guard.insert(
            (library_id, projection_scope.projection_version),
            SubTypeHintsCacheEntry { hints: hints.clone(), fetched_at: std::time::Instant::now() },
        );
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

    use super::{
        RevisionTableGraphContext, build_revision_table_graph_context,
        test_graph_input_hashes_by_chunk,
    };
    use crate::{
        infra::knowledge_rows::{KnowledgeChunkRow, KnowledgeStructuredBlockRow},
        shared::extraction::table_graph::build_graph_table_row_text,
    };

    fn make_chunk(normalized_text: &str) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
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

            window_text: None,

            raptor_level: None,
            occurred_at: None,
            occurred_until: None,
        }
    }

    fn make_text_chunk(normalized_text: &str) -> KnowledgeChunkRow {
        let mut chunk = make_chunk(normalized_text);
        chunk.chunk_kind = Some("paragraph".to_string());
        chunk
    }

    fn make_kind_chunk(chunk_kind: &str, chunk_index: i32) -> KnowledgeChunkRow {
        let mut chunk = make_text_chunk(&format!("record ordinal={chunk_index} field=value"));
        chunk.chunk_kind = Some(chunk_kind.to_string());
        chunk.chunk_index = chunk_index;
        chunk
    }

    fn make_block(
        block_id: Uuid,
        block_kind: &str,
        normalized_text: &str,
        parent_block_id: Option<Uuid>,
    ) -> KnowledgeStructuredBlockRow {
        KnowledgeStructuredBlockRow {
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

    fn make_structured_revision(
        source_format: &str,
    ) -> crate::infra::knowledge_rows::KnowledgeStructuredRevisionRow {
        let revision_id = Uuid::now_v7();
        crate::infra::knowledge_rows::KnowledgeStructuredRevisionRow {
            revision_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            preparation_state: "ready".to_string(),
            normalization_profile: "canonical".to_string(),
            source_format: source_format.to_string(),
            language_code: None,
            block_count: 0,
            chunk_count: 0,
            typed_fact_count: 0,
            outline_json: serde_json::json!({}),
            prepared_at: chrono::Utc::now(),
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

    #[test]
    fn graph_input_eligibility_uses_current_chunk_policy() {
        let clean = make_text_chunk(
            "Alpha service stores project settings and renders a concise operational summary.",
        );
        let noisy = make_text_chunk(concat!(
            "abCDEfgH ijKLMnOp qRStuVWx yzABcDef gHIjKLmn ",
            "opQRS7tu vwXYZabC deFGhIJk lmNOPqRs tuVWxyZa ",
            "bcDEFgHi jkLMNopQ rsTUVwxy zaBCDefG"
        ));

        let graph_inputs = test_graph_input_hashes_by_chunk(
            &[clean.clone(), noisy.clone()],
            &RevisionTableGraphContext::default(),
        );

        assert!(graph_inputs.contains_key(&clean.chunk_id));
        assert!(!graph_inputs.contains_key(&noisy.chunk_id));
    }

    #[test]
    fn graph_input_hash_changes_when_table_rendering_changes() {
        let table_block_id = Uuid::now_v7();
        let row_block_id = Uuid::now_v7();
        let mut row_chunk = make_chunk(
            "Sheet: products | Row 1 | Name: Alpha Suite | Owner: Platform | Internal Note: ignore",
        );
        row_chunk.support_block_ids = vec![row_block_id];

        let plain_input = test_graph_input_hashes_by_chunk(
            &[row_chunk.clone()],
            &RevisionTableGraphContext::default(),
        );
        let table_input = test_graph_input_hashes_by_chunk(
            &[row_chunk.clone()],
            &build_revision_table_graph_context(&[
                make_block(
                    row_block_id,
                    "table_row",
                    &row_chunk.normalized_text,
                    Some(table_block_id),
                ),
                make_block(
                    Uuid::now_v7(),
                    "metadata_block",
                    "Table Summary | Sheet: products | Column: Name | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 1 | Non-empty Count: 1 | Distinct Count: 1 | Most Frequent Count: 1 | Most Frequent Tie Count: 1 | Most Frequent Values: Alpha Suite",
                    Some(table_block_id),
                ),
            ]),
        );

        assert_ne!(plain_input.get(&row_chunk.chunk_id), table_input.get(&row_chunk.chunk_id));
    }

    #[test]
    fn record_stream_policy_keeps_retrieval_chunks_but_bounds_graph_source_units() {
        let mut chunks = vec![make_kind_chunk("source_profile", 0)];
        chunks.extend((1..=20).map(|index| make_kind_chunk("source_unit", index)));
        let structured_revision = make_structured_revision(
            crate::shared::extraction::record_jsonl::RECORD_JSONL_SOURCE_FORMAT,
        );

        let policy =
            super::build_graph_extraction_chunk_policy(Some(&structured_revision), &chunks, &[]);

        assert!(policy.is_record_stream());
        assert_eq!(policy.selected_source_unit_count(), 12);
        assert_eq!(super::record_stream_source_units_skipped(&chunks, &policy), 8);
    }

    #[test]
    fn record_stream_policy_requires_structured_revision_format() {
        let mut chunks = vec![make_kind_chunk("source_profile", 0)];
        chunks.extend((1..=20).map(|index| make_kind_chunk("source_unit", index)));

        let policy = super::build_graph_extraction_chunk_policy(None, &chunks, &[]);

        assert!(!policy.is_record_stream());
        assert_eq!(policy.selected_source_unit_count(), 0);
    }
}

#[cfg(test)]
mod image_checksum_tests {
    #[test]
    fn image_checksum_sha256_hex_format() {
        use hex;
        use sha2::{Digest, Sha256};
        let bytes: &[&[u8]] = &[b"image1", b"image2"];
        let mut sorted = bytes.to_vec();
        sorted.sort();
        let mut hasher = Sha256::new();
        for b in &sorted {
            hasher.update(b);
        }
        let result = hex::encode(hasher.finalize());
        assert_eq!(result.len(), 64);
        assert!(result.chars().all(|c| c.is_ascii_hexdigit()));
        let mut hasher2 = Sha256::new();
        for b in &sorted {
            hasher2.update(b);
        }
        let result2 = hex::encode(hasher2.finalize());
        assert_eq!(result, result2);
    }

    #[test]
    fn image_checksum_sort_is_deterministic() {
        use hex;
        use sha2::{Digest, Sha256};
        let order_a: &[&[u8]] = &[b"alpha", b"beta"];
        let order_b: &[&[u8]] = &[b"beta", b"alpha"];
        let hash = |slices: &[&[u8]]| {
            let mut v = slices.to_vec();
            v.sort();
            let mut h = Sha256::new();
            for b in &v {
                h.update(b);
            }
            hex::encode(h.finalize())
        };
        assert_eq!(hash(order_a), hash(order_b));
    }
}
