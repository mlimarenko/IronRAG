use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt, stream};
use tracing::warn;
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
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeStructuredBlockRow, KnowledgeStructuredRevisionRow,
        KnowledgeTechnicalFactRow,
    },
    infra::repositories::{
        self as repositories, catalog_repository,
        content_repository::{
            self, NewContentDocument, NewContentDocumentHead, NewContentRevision,
        },
    },
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        content::source_access::{
            derive_content_source_file_name, derive_storage_backed_content_file_name,
        },
        content::storage::ContentStorageService,
        graph::extract::{
            GraphExtractionSubTypeHintEntry, GraphExtractionSubTypeHintGroup,
            GraphExtractionSubTypeHints, build_graph_extraction_cache_fingerprint,
            extract_chunk_graph_candidates, extraction_lifecycle_from_record,
        },
        graph::projection::resolve_projection_scope,
        ingest::runtime::resolve_effective_runtime_task_context,
        ingest::service::{INGEST_STAGE_EXTRACT_CONTENT, LeaseAttemptCommand},
        ingest::structured_preparation::{
            PrepareStructuredRevisionCommand, StructuredPreparationService,
        },
        ingest::technical_facts::ExtractTechnicalFactsCommand,
        knowledge::service::{
            CreateKnowledgeChunkCommand, CreateKnowledgeDocumentCommand,
            CreateKnowledgeRevisionCommand, PromoteKnowledgeDocumentCommand,
        },
        ops::billing::CaptureGraphExtractionBillingCommand,
    },
    shared::extraction::file_extract::{
        FileExtractError, FileExtractionPlan, FileExtractionRequest, UploadAdmissionError,
        UploadFileKind, build_runtime_file_extraction_plan, validate_upload_file_admission,
    },
    shared::extraction::{
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
    ReprocessRevisionSource, RevisionAdmissionMetadata, RevisionGraphCandidateMaterialization,
    locate_chunk_offsets, map_revision_row, map_structured_revision_data, sha256_hex_text,
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
        // Vision binding is only needed for image files routed to the vision engine.
        // Text, markup, code, CSV, and Docling-owned document formats must not be
        // blocked by the absence of a vision binding.
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
        let plan = build_runtime_file_extraction_plan(FileExtractionRequest {
            gateway: state.llm_gateway.as_ref(),
            vision_provider: vision_provider.as_ref(),
            vision_api_key: vision_binding.as_ref().and_then(|binding| binding.api_key.as_deref()),
            vision_base_url: vision_binding
                .as_ref()
                .and_then(|binding| binding.provider_base_url.as_deref()),
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

    pub async fn render_revision_text_source(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Option<String>, ApiError> {
        let revision = state
            .arango_document_store
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
            .persist_inline_file_source(
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
            .arango_document_store
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
            .arango_document_store
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
            .persist_inline_file_source(
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
        // Resolve the library's chunking template so we apply the correct profile.
        let library_row =
            catalog_repository::get_library_by_id(&state.persistence.postgres, revision.library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let chunking_template = library_row
            .as_ref()
            .map(|lib| ChunkingTemplate::from_db_str(&lib.chunking_template))
            .unwrap_or(ChunkingTemplate::Naive);
        let preparation_service = StructuredPreparationService::with_template(chunking_template);
        let mut prepared = preparation_service
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
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
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
                window_text: pending_chunk.window_text.clone(),
                occurred_at: pending_chunk.occurred_at,
                occurred_until: pending_chunk.occurred_until,
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
        let structured_revision = state
            .arango_document_store
            .get_structured_revision(command.revision_id)
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
        let graph_chunk_policy = build_graph_extraction_chunk_policy(
            structured_revision.as_ref(),
            all_chunks.as_slice(),
            revision_facts.as_slice(),
        );
        let record_stream_source_units_skipped =
            record_stream_source_units_skipped(all_chunks.as_slice(), &graph_chunk_policy);
        let selected_graph_chunk_count = all_chunks
            .iter()
            .filter(|chunk| {
                build_graph_chunk_content(
                    chunk,
                    table_graph_context.profile_for_chunk(chunk),
                    table_graph_context.requires_row_only_graph(),
                    &graph_chunk_policy,
                )
                .is_some()
            })
            .count();
        if graph_chunk_policy.is_record_stream() {
            tracing::info!(
                revision_id = %command.revision_id,
                selected_graph_chunks = selected_graph_chunk_count,
                selected_source_units = graph_chunk_policy.selected_source_unit_count(),
                skipped_source_units = record_stream_source_units_skipped,
                "record-stream graph extraction chunk policy selected representative source units",
            );
        }

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

        let graph_runtime_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(
                state,
                command.library_id,
                AiBindingPurpose::ExtractGraph,
            )
            .await?
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "active extract_graph binding is required for graph extraction".to_string(),
                )
            })?;
        let provider_kind = graph_runtime_binding.provider_kind.clone();
        let model_name = graph_runtime_binding.model_name.clone();
        let request_size_soft_limit_bytes = state
            .resolve_settle_blockers_services
            .provider_failure_classification
            .request_size_soft_limit_bytes();
        let cache_plan = self
            .materialize_cached_graph_extractions(
                state,
                &command,
                &document,
                &revision,
                chunks.as_slice(),
                revision_facts.as_slice(),
                &library_extraction_prompt,
                &sub_type_hints,
                &table_graph_context,
                &graph_chunk_policy,
                &provider_kind,
                &model_name,
                &graph_runtime_binding,
                request_size_soft_limit_bytes,
            )
            .await?;
        let reused_chunk_ids = cache_plan.reused_chunk_ids;
        let reused_entity_count = cache_plan.reused_entity_count;
        let reused_relation_count = cache_plan.reused_relation_count;

        if !reused_chunk_ids.is_empty() {
            tracing::info!(
                revision_id = %command.revision_id,
                total_chunks = chunk_count,
                selected_graph_chunks = selected_graph_chunk_count,
                reused = reused_chunk_ids.len(),
                reused_entities = reused_entity_count,
                reused_relations = reused_relation_count,
                "graph extraction cache: reusing ready extraction output",
            );
        }

        // Filter out cache-covered chunks from the extraction loop — they are
        // already represented by ready runtime_graph_extraction records for
        // the current revision.
        let chunks: Vec<_> = chunks
            .into_iter()
            .filter(|chunk| !reused_chunk_ids.contains(&chunk.chunk_id))
            .collect();

        // Per-chunk graph extraction shares immutable state across the
        // in-flight futures. Wrapping the heavy structures in `Arc` keeps the
        // hot loop limited to the small per-chunk content and fact views.
        let document = std::sync::Arc::new(document);
        let revision = std::sync::Arc::new(revision);
        let revision_facts = std::sync::Arc::new(revision_facts);
        let library_extraction_prompt = std::sync::Arc::new(library_extraction_prompt);
        let sub_type_hints = std::sync::Arc::new(sub_type_hints);
        let table_graph_context = std::sync::Arc::new(table_graph_context);
        let graph_chunk_policy = std::sync::Arc::new(graph_chunk_policy);

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
            let graph_chunk_policy = std::sync::Arc::clone(&graph_chunk_policy);

            async move {
                let table_graph_profile = table_graph_context.profile_for_chunk(&chunk);
                let Some(chunk_content) =
                    build_graph_chunk_content(
                        &chunk,
                        table_graph_profile,
                        table_graph_context.requires_row_only_graph(),
                        &graph_chunk_policy,
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

        // Stream-fold the per-chunk results into one aggregate so each chunk's
        // small counters are consumed and dropped as soon as the future
        // completes.
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
            selected_graph_chunks: selected_graph_chunk_count,
            extracted_entities: extracted_entities.saturating_add(reused_entity_count),
            extracted_relations: extracted_relations.saturating_add(reused_relation_count),
            provider_kind: Some(provider_kind),
            model_name: Some(model_name),
            usage_json,
            reused_chunks: reused_chunk_ids.len(),
            reused_entities: reused_entity_count,
            reused_relations: reused_relation_count,
            record_stream_source_units_skipped,
        })
    }

    async fn materialize_cached_graph_extractions(
        &self,
        state: &AppState,
        command: &MaterializeRevisionGraphCandidatesCommand,
        document: &crate::infra::arangodb::document_store::KnowledgeDocumentRow,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunks: &[crate::infra::arangodb::document_store::KnowledgeChunkRow],
        revision_facts: &[TypedTechnicalFact],
        library_extraction_prompt: &Option<String>,
        sub_type_hints: &GraphExtractionSubTypeHints,
        table_graph_context: &RevisionTableGraphContext,
        graph_chunk_policy: &GraphExtractionChunkPolicy,
        provider_kind: &str,
        model_name: &str,
        runtime_binding: &ResolvedRuntimeBinding,
        request_size_soft_limit_bytes: usize,
    ) -> Result<GraphExtractionCachePlan, ApiError> {
        let mut plan = GraphExtractionCachePlan::default();
        for chunk in chunks {
            let Some(chunk_content) = build_graph_chunk_content(
                chunk,
                table_graph_context.profile_for_chunk(chunk),
                table_graph_context.requires_row_only_graph(),
                graph_chunk_policy,
            ) else {
                continue;
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
                library_extraction_prompt.clone(),
                sub_type_hints.clone(),
            );
            let fingerprint = build_graph_extraction_cache_fingerprint(
                &request,
                runtime_binding,
                request_size_soft_limit_bytes,
            );
            let text_checksum = sha256_hex_text(&chunk.normalized_text);
            let Some(record) =
                crate::infra::repositories::find_ready_runtime_graph_extraction_record_by_cache_key(
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
                        "graph_cache: find_ready_runtime_graph_extraction_record_by_cache_key",
                    )
                })?
            else {
                continue;
            };

            if record.chunk_id != chunk.chunk_id {
                let raw_output_json =
                    graph_cache_reuse_raw_output(&record, command.revision_id, command.attempt_id);
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
                        normalized_output_json: record.normalized_output_json.clone(),
                        glean_pass_count: 0,
                        error_message: None,
                    },
                )
                .await
                .map_err(|e| {
                    ApiError::internal_with_log(
                        e,
                        "graph_cache: create_runtime_graph_extraction_record",
                    )
                })?;
            } else {
                let lifecycle = extraction_lifecycle_from_record(&record);
                if lifecycle.revision_id.is_some()
                    && lifecycle.revision_id != Some(command.revision_id)
                {
                    continue;
                }
            }

            let (entity_count, relation_count) =
                graph_candidate_counts(&record.normalized_output_json);
            plan.reused_entity_count = plan.reused_entity_count.saturating_add(entity_count);
            plan.reused_relation_count = plan.reused_relation_count.saturating_add(relation_count);
            plan.reused_chunk_ids.insert(chunk.chunk_id);
        }
        Ok(plan)
    }
}

#[derive(Debug, Default)]
struct GraphExtractionCachePlan {
    reused_chunk_ids: BTreeSet<Uuid>,
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
        // Optimistic housekeeping: drop stale entries when the cache
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

    use super::{
        RevisionTableGraphContext, build_revision_table_graph_context,
        test_graph_input_hashes_by_chunk,
    };
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

    fn make_structured_revision(
        source_format: &str,
    ) -> crate::infra::arangodb::document_store::KnowledgeStructuredRevisionRow {
        let revision_id = Uuid::now_v7();
        crate::infra::arangodb::document_store::KnowledgeStructuredRevisionRow {
            key: revision_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
