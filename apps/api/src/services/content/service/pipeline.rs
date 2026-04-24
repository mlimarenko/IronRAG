use std::time::Instant;

use super::InlineMutationContext;

use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::TypedTechnicalFact,
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
    },
    infra::repositories,
    interfaces::http::router_support::ApiError,
    services::{
        graph::extract::{
            GraphExtractionRequest, GraphExtractionStructuredChunkContext,
            GraphExtractionSubTypeHints, GraphExtractionTechnicalFact,
        },
        ingest::service::{
            FinalizeAttemptCommand, INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EMBED_CHUNK,
            INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH,
            INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_FINALIZING,
            INGEST_STAGE_PREPARE_STRUCTURE, RecordStageEventCommand,
        },
        ops::billing::CaptureIngestAttemptBillingCommand,
    },
    shared::extraction::{
        file_extract::build_inline_text_extraction_plan,
        table_graph::{TableGraphProfile, build_graph_table_row_text},
        table_summary::is_table_summary_text,
    },
};

use super::{
    AcceptMutationCommand, AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
    ContentMutationAdmission, ContentService, CreateDocumentAdmission, CreateMutationItemCommand,
    CreateRevisionCommand, EditInlineMutationCommand, MaterializeRevisionGraphCandidatesCommand,
    MaterializeWebCaptureCommand, MaterializedWebCapture, PromoteHeadCommand,
    ReconcileFailedIngestMutationCommand, ReplaceInlineMutationCommand, RevisionAdmissionMetadata,
    UpdateMutationCommand, UpdateMutationItemCommand, UploadInlineDocumentCommand,
    edited_markdown_file_name, infer_inline_mime_type, merge_appended_text, sha256_hex_bytes,
    sha256_hex_text, source_uri_for_inline_payload,
};

pub(super) fn build_graph_chunk_content(
    chunk: &KnowledgeChunkRow,
    table_graph_profile: Option<&TableGraphProfile>,
    row_only_table_graph: bool,
) -> Option<String> {
    if row_only_table_graph && chunk.chunk_kind.as_deref() != Some("table_row") {
        return None;
    }
    if chunk.chunk_kind.as_deref() == Some("metadata_block")
        && is_table_summary_text(&chunk.normalized_text)
    {
        return None;
    }
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return build_graph_table_row_text(&chunk.normalized_text, table_graph_profile);
    }

    Some(chunk.normalized_text.clone())
}

pub(super) fn build_canonical_graph_extraction_request(
    document: &KnowledgeDocumentRow,
    revision: &KnowledgeRevisionRow,
    chunk: &KnowledgeChunkRow,
    chunk_content: String,
    technical_facts: &[TypedTechnicalFact],
    attempt_id: Option<Uuid>,
    library_extraction_prompt: Option<String>,
    sub_type_hints: GraphExtractionSubTypeHints,
) -> GraphExtractionRequest {
    let graph_technical_facts = if chunk.chunk_kind.as_deref() == Some("table_row") {
        Vec::new()
    } else {
        technical_facts
            .iter()
            .map(|fact| GraphExtractionTechnicalFact {
                fact_kind: fact.fact_kind.as_str().to_string(),
                canonical_value: fact.canonical_value.canonical_string(),
                display_value: fact.display_value.clone(),
                qualifiers: fact.qualifiers.clone(),
            })
            .collect()
    };

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
            content: chunk_content,
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
        technical_facts: graph_technical_facts,
        revision_id: Some(revision.revision_id),
        activated_by_attempt_id: attempt_id,
        resume_hint: None,
        library_extraction_prompt,
        sub_type_hints,
    }
}

pub(super) fn typed_fact_supports_chunk(
    fact: &TypedTechnicalFact,
    chunk: &KnowledgeChunkRow,
) -> bool {
    fact.support_chunk_ids.contains(&chunk.chunk_id)
        || fact.support_block_ids.iter().any(|block_id| chunk.support_block_ids.contains(block_id))
}

impl ContentService {
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
        let checksum_value = format!("sha256:{file_checksum}");
        // Content-identity dedup. Before touching storage or the
        // mutation pipeline, refuse uploads whose SHA-256 already
        // matches a non-deleted document in this library. Operators
        // asked for an explicit signal (not a silent ack) because a
        // previous crawl had thousands of URL variants collapsing onto
        // the same page body and quietly accepting them hid the
        // duplication until the docs table was full of $0.000 ghosts.
        if let Some(existing_document_id) =
            repositories::content_repository::find_active_document_by_library_checksum(
                &state.persistence.postgres,
                command.library_id,
                &checksum_value,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            return Err(ApiError::Conflict(format!(
                "duplicate content: identical file already exists in this library as document {existing_document_id}"
            )));
        }
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
        // Content-dedup within the library. Web sites routinely expose
        // the same body under many URL variants (viewlabel.action with
        // different query strings, labels/pages mirrored across spaces,
        // …) and without this check we end up with a thousand
        // `$0.000`-cost ghost documents pointing at the same bytes.
        // Lookup is best-effort (not advisory-locked) — a simultaneous
        // re-crawl of the same variant inside the same run is already
        // filtered by canonical-URL dedup in recursive.rs; what this
        // catches is the variant-on-variant collapse.
        if let Some(existing_document_id) =
            repositories::content_repository::find_active_document_by_library_checksum(
                &state.persistence.postgres,
                command.library_id,
                &command.checksum,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            let mutation_item = self
                .create_mutation_item(
                    state,
                    CreateMutationItemCommand {
                        mutation_id: command.mutation_id,
                        document_id: Some(existing_document_id),
                        base_revision_id: None,
                        result_revision_id: None,
                        item_state: "skipped".to_string(),
                        message: Some(format!(
                            "skipped: duplicate content already ingested as document {existing_document_id}"
                        )),
                    },
                )
                .await?;
            return Ok(MaterializedWebCapture::DuplicateContent {
                existing_document_id,
                mutation_item,
            });
        }

        let document = match self
            .get_document_by_external_key(state, command.library_id, &command.final_url)
            .await?
        {
            Some(document) => document,
            None => {
                self.create_document(
                    state,
                    super::CreateDocumentCommand {
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
                crate::services::ingest::service::AdmitIngestJobCommand {
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

        Ok(MaterializedWebCapture::Ingested { document, revision, mutation_item, job_id: job.id })
    }

    pub async fn append_inline_mutation(
        &self,
        state: &AppState,
        command: AppendInlineMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let document_context =
            self.load_editable_document_context(state, command.document_id).await?;
        let merged_text =
            merge_appended_text(&document_context.current_content, &command.appended_text);
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
                        mime_type: document_context.mime_type,
                        byte_size: i64::try_from(merged_text.len()).unwrap_or(i64::MAX),
                        title: document_context.title,
                        language_code: document_context.language_code,
                        source_uri: Some(source_uri_for_inline_payload(
                            "append",
                            source_identity.as_deref(),
                            None,
                        )),
                        storage_key: None,
                    }),
                    parent_async_operation_id: None,
                },
            )
            .await?;
        self.materialize_inline_text_mutation(state, &admission, merged_text).await
    }

    pub async fn edit_inline_mutation(
        &self,
        state: &AppState,
        command: EditInlineMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let document_context =
            self.load_editable_document_context(state, command.document_id).await?;
        let markdown = command.markdown;
        if markdown.trim().is_empty() {
            return Err(ApiError::BadRequest("edited markdown must not be empty".to_string()));
        }

        let file_checksum = sha256_hex_bytes(markdown.as_bytes());
        let file_name =
            edited_markdown_file_name(document_context.title.as_deref(), command.document_id);
        let source_identity = command
            .source_identity
            .clone()
            .or_else(|| Some(format!("edit-inline:{file_checksum}:{}", command.document_id)));
        let accept_command = AcceptMutationCommand {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            operation_kind: "edit".to_string(),
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

        self.ensure_document_accepts_new_mutation(state, command.document_id, "edit").await?;
        let storage_key = self
            .persist_inline_file_source(
                state,
                command.workspace_id,
                command.library_id,
                &file_name,
                &format!("sha256:{file_checksum}"),
                markdown.as_bytes(),
            )
            .await?;
        let source_uri = source_uri_for_inline_payload("edit", None, Some(&file_name));
        self.admit_mutation(
            state,
            AdmitMutationCommand {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                document_id: command.document_id,
                operation_kind: "edit".to_string(),
                idempotency_key: command.idempotency_key,
                requested_by_principal_id: command.requested_by_principal_id,
                request_surface: command.request_surface,
                source_identity,
                revision: Some(RevisionAdmissionMetadata {
                    content_source_kind: "edit".to_string(),
                    checksum: format!("sha256:{file_checksum}"),
                    mime_type: "text/markdown".to_string(),
                    byte_size: i64::try_from(markdown.len()).unwrap_or(i64::MAX),
                    title: document_context.title,
                    language_code: document_context.language_code,
                    source_uri: Some(source_uri),
                    storage_key: Some(storage_key),
                }),
                parent_async_operation_id: None,
            },
        )
        .await
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
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
                parent_async_operation_id: None,
            },
        )
        .await
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
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: None,
                },
            )
            .await?;
        let stage_start = Instant::now();
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
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: Some(stage_start.elapsed().as_millis() as i64),
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
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: None,
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
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: Some(preparation.prepare_structure_elapsed_ms),
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
                    stage_state: "started".to_string(),
                    message: Some("persisting content chunks".to_string()),
                    details_json: serde_json::json!({
                        "revisionId": context.revision_id,
                    }),
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: None,
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
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: Some(preparation.chunk_content_elapsed_ms),
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
                    stage_state: "started".to_string(),
                    message: Some(
                        "extracting technical facts from structured revision".to_string(),
                    ),
                    details_json: serde_json::json!({
                        "revisionId": context.revision_id,
                    }),
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: None,
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
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: Some(preparation.extract_technical_facts_elapsed_ms),
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
        if let Err(error) = self
            .converge_document_technical_facts(
                state,
                context.document_id,
                Some(context.revision_id),
            )
            .await
        {
            warn!(
                document_id = %context.document_id,
                revision_id = %context.revision_id,
                mutation_id = %context.mutation_id,
                ?error,
                "post-head-promotion technical fact convergence failed after inline mutation commit"
            );
        }
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

    async fn run_inline_post_chunk_pipeline(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_id: Uuid,
    ) -> Result<(), ApiError> {
        // --- Stage: embed_chunk ------------------------------------------
        // Mirrors the background-ingest worker: embed this revision's
        // chunks synchronously using the library's EmbedChunk binding so
        // the vector lane has queryable rows before we even flip
        // `vector_state` to ready. Prior to this stage the pipeline was
        // a no-op that still promoted vector_state and gave every query
        // a silent zero-vector-hits failure mode.
        state
            .canonical_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand {
                    attempt_id,
                    stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
                    stage_state: "started".to_string(),
                    message: Some("embedding chunks".to_string()),
                    details_json: serde_json::json!({
                        "revisionId": context.revision_id,
                    }),
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: None,
                },
            )
            .await?;
        let embed_chunk_start = Instant::now();
        let embed_chunk_result = state
            .canonical_services
            .search
            .embed_chunks_for_revision(state, context.library_id, context.revision_id)
            .await;
        let embed_chunk_elapsed_ms = Some(embed_chunk_start.elapsed().as_millis() as i64);
        let embed_chunk_success = match &embed_chunk_result {
            Ok(outcome) => {
                if let (Some(provider), Some(model), Some(usage_json)) = (
                    outcome.provider_kind.clone(),
                    outcome.model_name.clone(),
                    outcome.usage_json.clone(),
                ) {
                    if let Err(error) = state
                        .canonical_services
                        .billing
                        .capture_ingest_attempt(
                            state,
                            CaptureIngestAttemptBillingCommand {
                                workspace_id: context.workspace_id,
                                library_id: context.library_id,
                                attempt_id,
                                binding_id: None,
                                provider_kind: provider,
                                model_name: model,
                                call_kind: "embed_chunk".to_string(),
                                usage_json,
                            },
                        )
                        .await
                    {
                        warn!(
                            attempt_id = %attempt_id,
                            ?error,
                            "embed_chunk billing capture failed; continuing ingest",
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
                            stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
                            stage_state: "completed".to_string(),
                            message: Some("chunk embeddings persisted".to_string()),
                            details_json: serde_json::json!({
                                "chunksEmbedded": outcome.chunks_embedded,
                                "providerKind": outcome.provider_kind,
                                "modelName": outcome.model_name,
                            }),
                            provider_kind: outcome.provider_kind.clone(),
                            model_name: outcome.model_name.clone(),
                            prompt_tokens: outcome.prompt_tokens,
                            completion_tokens: outcome.completion_tokens,
                            total_tokens: outcome.total_tokens,
                            cached_tokens: None,
                            estimated_cost: None,
                            currency_code: None,
                            elapsed_ms: embed_chunk_elapsed_ms,
                        },
                    )
                    .await?;
                true
            }
            Err(error) => {
                warn!(
                    attempt_id = %attempt_id,
                    revision_id = %context.revision_id,
                    ?error,
                    "chunk embedding failed; vector lane will remain empty for this revision",
                );
                state
                    .canonical_services
                    .ingest
                    .record_stage_event(
                        state,
                        RecordStageEventCommand {
                            attempt_id,
                            stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
                            stage_state: "failed".to_string(),
                            message: Some("chunk embedding failed".to_string()),
                            details_json: serde_json::json!({
                                "error": format!("{error:#}"),
                            }),
                            provider_kind: None,
                            model_name: None,
                            prompt_tokens: None,
                            completion_tokens: None,
                            total_tokens: None,
                            cached_tokens: None,
                            estimated_cost: None,
                            currency_code: None,
                            elapsed_ms: embed_chunk_elapsed_ms,
                        },
                    )
                    .await?;
                false
            }
        };
        drop(embed_chunk_result);

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
                    provider_kind: None,
                    model_name: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    cached_tokens: None,
                    estimated_cost: None,
                    currency_code: None,
                    elapsed_ms: None,
                },
            )
            .await?;
        let extract_start = Instant::now();
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
        let extract_elapsed_ms = extract_start.elapsed().as_millis() as i64;
        let mut graph_ready = false;
        let mut graph_failure: Option<String> = None;

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
                        // Graph node/edge embedding usage is captured as
                        // its own `embed_graph` billing call_kind. Earlier
                        // versions filed this under the `embed_chunk`
                        // stage name, conflating graph embedding with
                        // (previously non-existent) chunk embedding.
                        if let Some(embedding_usage) = graph_outcome.embedding_usage {
                            let embed_provider = embedding_usage.provider_kind.clone();
                            let embed_model = embedding_usage.model_name.clone();

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
                                        provider_kind: embed_provider.clone().unwrap_or_default(),
                                        model_name: embed_model.clone().unwrap_or_default(),
                                        call_kind: "embed_graph".to_string(),
                                        usage_json: embedding_usage.into_usage_json(),
                                    },
                                )
                                .await
                            {
                                warn!(
                                    attempt_id = %attempt_id,
                                    ?error,
                                    "embed_graph billing capture failed; continuing ingest",
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
                                        "providerKind": graph_materialization.provider_kind,
                                        "modelName": graph_materialization.model_name,
                                    }),
                                    provider_kind: graph_materialization.provider_kind.clone(),
                                    model_name: graph_materialization.model_name.clone(),
                                    prompt_tokens: graph_materialization.usage_json.get("prompt_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                    completion_tokens: graph_materialization.usage_json.get("completion_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                    total_tokens: graph_materialization.usage_json.get("total_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                    cached_tokens: None,
                                    estimated_cost: None,
                                    currency_code: None,
                                    elapsed_ms: Some(extract_elapsed_ms),
                                },
                            )
                            .await?;
                    }
                    Err(error) => {
                        graph_failure = Some(format!("graph reconcile failed: {error:#}"));
                        // extract_graph itself succeeded — failure is in the
                        // reconcile/graph-embed phase. Close extract_graph
                        // normally so the UI shows where the pipeline
                        // actually broke.
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
                                        format!("graph reconcile failed: {error:#}"),
                                    ),
                                    details_json: serde_json::json!({
                                        "chunksProcessed": graph_materialization.chunk_count,
                                        "extractedEntityCandidates": graph_materialization.extracted_entities,
                                        "extractedRelationCandidates": graph_materialization.extracted_relations,
                                        "providerKind": graph_materialization.provider_kind,
                                        "modelName": graph_materialization.model_name,
                                    }),
                                    provider_kind: graph_materialization.provider_kind.clone(),
                                    model_name: graph_materialization.model_name.clone(),
                                    prompt_tokens: graph_materialization.usage_json.get("prompt_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                    completion_tokens: graph_materialization.usage_json.get("completion_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                    total_tokens: graph_materialization.usage_json.get("total_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                    cached_tokens: None,
                                    estimated_cost: None,
                                    currency_code: None,
                                    elapsed_ms: Some(extract_elapsed_ms),
                                },
                            )
                            .await?;
                    }
                }
            }
            Err(error) => {
                graph_failure = Some(format!("graph candidate extraction failed: {error:#}"));
                state
                    .canonical_services
                    .ingest
                    .record_stage_event(
                        state,
                        RecordStageEventCommand {
                            attempt_id,
                            stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                            stage_state: "failed".to_string(),
                            message: Some("inline graph candidate extraction failed".to_string()),
                            details_json: serde_json::json!({
                                "graphReady": false,
                                "error": error.to_string(),
                            }),
                            provider_kind: None,
                            model_name: None,
                            prompt_tokens: None,
                            completion_tokens: None,
                            total_tokens: None,
                            cached_tokens: None,
                            estimated_cost: None,
                            currency_code: None,
                            elapsed_ms: Some(extract_elapsed_ms),
                        },
                    )
                    .await?;
            }
        }

        // Fail-fast: graph_ready already encodes
        // (graph_contribution_count > 0 && projection.graph_status == "ready").
        // Anything else is a hard failure for this pipeline — there is no
        // half-complete state and no silent degradation to readable.
        if !graph_ready && graph_failure.is_none() {
            graph_failure =
                Some("graph reconcile produced no contributions for this revision".to_string());
        }

        let revision = state
            .arango_document_store
            .get_revision(context.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_revision", context.revision_id)
            })?;
        let now = Utc::now();
        let graph_state_label = if graph_ready { "ready" } else { "failed" };
        let vector_state_label = if embed_chunk_success { "ready" } else { "failed" };
        let vector_ready_at =
            if embed_chunk_success { revision.vector_ready_at.or(Some(now)) } else { None };
        state
            .arango_document_store
            .update_revision_readiness(
                revision.revision_id,
                &revision.text_state,
                vector_state_label,
                graph_state_label,
                revision.text_readable_at,
                vector_ready_at,
                revision.graph_ready_at.or(graph_ready.then_some(now)),
                revision.superseded_by_revision_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        if let Some(reason) = graph_failure {
            return Err(ApiError::internal_with_log(&reason, "inline graph pipeline failed"));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::build_graph_chunk_content;
    use crate::infra::arangodb::document_store::KnowledgeChunkRow;

    fn make_chunk(chunk_kind: &str, normalized_text: &str) -> KnowledgeChunkRow {
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
            chunk_kind: Some(chunk_kind.to_string()),
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

    #[test]
    fn build_graph_chunk_content_skips_table_summary_metadata_chunks() {
        let chunk = make_chunk(
            "metadata_block",
            "Table Summary | Sheet: products | Column: Stock | Value Kind: numeric | Row Count: 100 | Average: 545.71",
        );

        assert_eq!(build_graph_chunk_content(&chunk, None, false), None);
    }

    #[test]
    fn build_graph_chunk_content_skips_heading_chunks_for_row_only_table_revisions() {
        let chunk = make_chunk("heading", "test1");

        assert_eq!(build_graph_chunk_content(&chunk, None, true), None);
    }
}
