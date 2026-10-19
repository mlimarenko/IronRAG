use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use super::InlineMutationContext;
use super::revision::{InlineFileSourceInput, PersistedRevisionSourceGuard};

use chrono::Utc;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::TypedTechnicalFact,
    infra::repositories::{self, admission_repository, ingest_repository},
    infra::{
        knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
        knowledge_rows::{KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow},
    },
    interfaces::http::router_support::{ApiError, map_runtime_lifecycle_error},
    services::{
        content::storage::ContentStorageService,
        graph::extract::{
            GraphExtractionRequest, GraphExtractionStructuredChunkContext,
            GraphExtractionSubTypeHints, GraphExtractionTechnicalFact,
        },
        ingest::service::{
            INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EMBED_CHUNK, INGEST_STAGE_EXTRACT_CONTENT,
            INGEST_STAGE_EXTRACT_GRAPH, INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
            INGEST_STAGE_PREPARE_STRUCTURE, RecordStageEventCommand,
        },
        ops::billing::CaptureIngestAttemptBillingCommand,
        query::vector_dimensions::{
            ensure_active_embedding_profile_key, invalidate_library_embedding_profile_inventory,
        },
    },
    shared::extraction::{
        file_extract::{
            build_inline_text_extraction_plan, build_inline_text_extraction_plan_for_source,
        },
        record_jsonl::project_record_unit_values_for_graph,
        table_graph::{TableGraphProfile, build_graph_table_row_text},
        table_summary::is_table_summary_text,
        text_quality::is_graph_extraction_text_eligible,
    },
};

use super::{
    AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
    ContentMutationAdmission, ContentService, CreateDocumentAdmission, GRAPH_STATE_DEGRADED,
    MaterializeRevisionGraphCandidatesCommand, MaterializeWebCaptureCommand,
    MaterializedWebCapture, ReplaceInlineMutationCommand, RevisionAdmission, UpdateMutationCommand,
    UploadInlineDocumentCommand, graph_extract_success_message,
    graph_state_after_successful_extract, infer_inline_mime_type, map_mutation_item_row,
    merge_appended_bytes, sha256_hex_bytes, source_uri_for_inline_payload,
};

struct InlineAttemptHeartbeatGuard {
    running: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineContentPipelineOutcome {
    Applied,
    AuthorityLost,
}

#[derive(Debug)]
struct InlinePipelineFailure {
    error: ApiError,
    delete_vectors: bool,
}

impl InlinePipelineFailure {
    const fn new(error: ApiError, delete_vectors: bool) -> Self {
        Self { error, delete_vectors }
    }
}

impl From<ApiError> for InlinePipelineFailure {
    fn from(error: ApiError) -> Self {
        Self::new(error, false)
    }
}

impl Drop for InlineAttemptHeartbeatGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

fn spawn_inline_attempt_heartbeat(
    state: &AppState,
    attempt_id: Uuid,
    cancellation_token: CancellationToken,
) -> InlineAttemptHeartbeatGuard {
    let running = Arc::new(AtomicBool::new(true));
    let heartbeat_running = Arc::clone(&running);
    let heartbeat_postgres = state.persistence.heartbeat_postgres.clone();
    let heartbeat_interval =
        Duration::from_secs(state.settings.ingestion_worker_heartbeat_interval_seconds.max(1));
    tokio::spawn(async move {
        while heartbeat_running.load(Ordering::Relaxed) {
            time::sleep(heartbeat_interval).await;
            if !heartbeat_running.load(Ordering::Relaxed) {
                break;
            }
            match ingest_repository::touch_attempt_heartbeat_and_load_job_state(
                &heartbeat_postgres,
                attempt_id,
                None,
            )
            .await
            {
                Ok(Some(queue_state)) if queue_state == "leased" => {}
                Ok(queue_state) => {
                    cancellation_token.cancel();
                    warn!(
                        attempt_id = %attempt_id,
                        ?queue_state,
                        "inline ingest attempt heartbeat observed lost lease",
                    );
                    break;
                }
                Err(error) => {
                    warn!(
                        attempt_id = %attempt_id,
                        ?error,
                        "failed to touch inline ingest attempt heartbeat",
                    );
                }
            }
        }
    });
    InlineAttemptHeartbeatGuard { running }
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
        let file_name = command.file_name.trim().to_string();
        let replacement = command.replacement_identity()?;
        let storage_lock =
            repositories::content_mutation_repository::acquire_content_library_storage_lock(
                &state.persistence.postgres,
                command.library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // Exact target, idempotency and checksum decisions belong to canonical
        // admission under its library lock, not to a source-less preflight read.
        let storage_key = ContentStorageService::build_revision_storage_key(
            command.workspace_id,
            command.library_id,
            &file_name,
            &checksum_value,
        );
        let mut staged_blob = PersistedRevisionSourceGuard::arm(
            state,
            command.workspace_id,
            command.library_id,
            storage_key.clone(),
            storage_lock,
        );
        let persisted_storage_key = self
            .persist_inline_file_source(
                state,
                InlineFileSourceInput {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    file_name: &file_name,
                    checksum: &checksum_value,
                    file_bytes: &command.file_bytes,
                },
            )
            .await?;
        debug_assert_eq!(
            persisted_storage_key, storage_key,
            "persisted key must match the key armed on the guard",
        );
        // IRG-203 SITE-6: the staged blob is on disk and the compensating guard
        // is armed, but the admission that would commit a revision referencing
        // it has not run. A one-shot test probe parks the caller in exactly that
        // window so a test can abort and prove the guard reclaims the orphan.
        // Compiled out of production builds; a no-op when no probe is installed.
        #[cfg(feature = "test-support")]
        {
            crate::shared::cancellation::one_shot_pause_probe::pause().await;
        }
        let admission = self
            .admit_document_source(
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
                    revision: Some(RevisionAdmission {
                        content_source_kind: "upload".to_string(),
                        replacement,
                        visibility_intent_id: None,
                        base_revision_id: None,
                        storage_key: Some(storage_key),
                    }),
                    parent_external_key: command.parent_external_key,
                },
                Some(&command.file_bytes),
            )
            .await?;
        // Admission committed a revision that references the blob;
        // disarm releases the library storage lock.
        staged_blob.disarm();
        Ok(admission)
    }

    pub async fn materialize_web_capture(
        &self,
        state: &AppState,
        command: MaterializeWebCaptureCommand,
    ) -> Result<MaterializedWebCapture, ApiError> {
        let replacement = ironrag_contracts::documents::ReplacementIdentity {
            checksum: command.checksum,
            byte_size: command.byte_size,
            mime_type: command.mime_type,
            file_name: command.file_name,
            title: command.title,
            language_code: None,
            source_uri: Some(command.final_url.clone()),
            document_hint: None,
        };
        let admission = admission_repository::admit_web_capture_materialization_with_failpoint(
            &state.persistence.postgres,
            &admission_repository::WebCaptureMaterializationAdmissionRequest {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                mutation_id: command.mutation_id,
                final_url: command.final_url,
                replacement,
                storage_key: command.storage_key,
                requested_by_principal_id: command.requested_by_principal_id,
                priority: 100,
            },
            None,
        )
        .await
        .map_err(|error| {
            ApiError::internal_with_log(error, "web capture materialization admission failed")
        })?
        .into_bundle();
        match admission {
            admission_repository::WebCaptureMaterializationAdmissionBundle::Ingested(bundle) => {
                let revision = bundle.revision.ok_or(ApiError::Internal)?;
                let item = bundle.item.ok_or(ApiError::Internal)?;
                let job = bundle.job.ok_or(ApiError::Internal)?;
                Ok(MaterializedWebCapture::Ingested {
                    document: super::map_document_row(bundle.document),
                    revision: Box::new(super::map_revision_row(revision)),
                    mutation_item: map_mutation_item_row(item),
                    job_id: job.id,
                })
            }
            admission_repository::WebCaptureMaterializationAdmissionBundle::DuplicateContent {
                existing_document_id,
                item,
            } => Ok(MaterializedWebCapture::DuplicateContent {
                existing_document_id,
                mutation_item: map_mutation_item_row(item),
            }),
        }
    }

    pub async fn append_inline_mutation(
        &self,
        state: &AppState,
        command: AppendInlineMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        if command.appended_text.trim().is_empty() {
            return Err(ApiError::BadRequest("appendedText must not be empty".to_string()));
        }
        let source_identity = command.source_identity.clone();
        let appendable = self
            .load_appendable_document_source(
                state,
                command.workspace_id,
                command.library_id,
                command.document_id,
                command.base_revision_id,
            )
            .await?;
        let source_file_name = appendable.replacement.title.clone();
        let source_mime_type = appendable.replacement.mime_type.clone();
        let merged_bytes =
            merge_appended_bytes(&appendable.raw_bytes, &command.appended_text, &source_mime_type);
        if merged_bytes.is_empty() {
            return Err(ApiError::BadRequest(
                "append produced no content — appendedText must not be empty".to_string(),
            ));
        }
        let merged_text = String::from_utf8(merged_bytes.clone()).map_err(|_| {
            ApiError::BadRequest(
                "append produced non-utf8 content — only text-like sources can be appended"
                    .to_string(),
            )
        })?;
        let merged_checksum = sha256_hex_bytes(&merged_bytes);
        let storage_file_name = appendable.replacement.file_name.clone();
        let storage_lock =
            repositories::content_mutation_repository::acquire_content_library_storage_lock(
                &state.persistence.postgres,
                command.library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let storage_key = ContentStorageService::build_revision_storage_key(
            command.workspace_id,
            command.library_id,
            &storage_file_name,
            &format!("sha256:{merged_checksum}"),
        );
        let mut staged_blob = PersistedRevisionSourceGuard::arm(
            state,
            command.workspace_id,
            command.library_id,
            storage_key.clone(),
            storage_lock,
        );
        let persisted_storage_key = self
            .persist_inline_file_source(
                state,
                InlineFileSourceInput {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    file_name: &storage_file_name,
                    checksum: &format!("sha256:{merged_checksum}"),
                    file_bytes: &merged_bytes,
                },
            )
            .await?;
        debug_assert_eq!(
            persisted_storage_key, storage_key,
            "persisted key must match the key armed on the guard",
        );
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
                    revision: Some(RevisionAdmission {
                        content_source_kind: "append".to_string(),
                        replacement: ironrag_contracts::documents::ReplacementIdentity {
                            file_name: storage_file_name.clone(),
                            checksum: format!("sha256:{merged_checksum}"),
                            mime_type: source_mime_type.clone(),
                            byte_size: i64::try_from(merged_bytes.len()).unwrap_or(i64::MAX),
                            title: appendable.replacement.title,
                            language_code: appendable.replacement.language_code,
                            source_uri: appendable.replacement.source_uri,
                            document_hint: appendable.replacement.document_hint,
                        },
                        visibility_intent_id: None,
                        base_revision_id: Some(command.base_revision_id),
                        storage_key: Some(storage_key),
                    }),
                    parent_async_operation_id: None,
                },
            )
            .await?;
        // `admit_mutation` committed a revision that references the blob;
        // disarm releases the library storage lock.
        staged_blob.disarm();
        if admission.replayed {
            return Ok(admission);
        }
        self.materialize_inline_text_mutation(
            state,
            &admission,
            merged_text,
            source_file_name,
            Some(source_mime_type),
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
        let replacement = command.replacement_identity()?;
        let storage_lock =
            repositories::content_mutation_repository::acquire_content_library_storage_lock(
                &state.persistence.postgres,
                command.library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let storage_key = ContentStorageService::build_revision_storage_key(
            command.workspace_id,
            command.library_id,
            &replacement.file_name,
            &format!("sha256:{file_checksum}"),
        );
        let mut staged_blob = PersistedRevisionSourceGuard::arm(
            state,
            command.workspace_id,
            command.library_id,
            storage_key.clone(),
            storage_lock,
        );
        let persisted_storage_key = self
            .persist_inline_file_source(
                state,
                InlineFileSourceInput {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    file_name: &replacement.file_name,
                    checksum: &format!("sha256:{file_checksum}"),
                    file_bytes: &command.file_bytes,
                },
            )
            .await?;
        debug_assert_eq!(
            persisted_storage_key, storage_key,
            "persisted key must match the key armed on the guard",
        );
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
                    source_identity: command.source_identity,
                    revision: Some(RevisionAdmission {
                        content_source_kind: "replace".to_string(),
                        replacement,
                        visibility_intent_id: command.visibility_intent_id,
                        base_revision_id: None,
                        storage_key: Some(storage_key),
                    }),
                    parent_async_operation_id: None,
                },
            )
            .await?;
        // `admit_mutation` committed a revision that references the blob;
        // disarm releases the library storage lock.
        staged_blob.disarm();
        Ok(admission)
    }
}

impl UploadInlineDocumentCommand {
    /// The effective creation metadata, shared by automatic keys and admission.
    pub(crate) fn replacement_identity(
        &self,
    ) -> Result<ironrag_contracts::documents::ReplacementIdentity, ApiError> {
        let file_name = self.file_name.trim().to_owned();
        let title = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| file_name.clone(), ToString::to_string);
        ironrag_contracts::documents::ReplacementIdentity {
            checksum: format!("sha256:{}", sha256_hex_bytes(&self.file_bytes)),
            byte_size: i64::try_from(self.file_bytes.len())
                .map_err(|_| ApiError::BadRequest("upload is too large".to_owned()))?,
            mime_type: infer_inline_mime_type(
                self.mime_type.as_deref(),
                Some(&file_name),
                "upload",
            ),
            source_uri: Some(source_uri_for_inline_payload(
                "upload",
                self.source_identity.as_deref(),
                Some(&file_name),
            )),
            file_name,
            title: Some(title),
            language_code: None,
            document_hint: self.document_hint.clone(),
        }
        .normalize()
        .map_err(|error| ApiError::BadRequest(error.to_string()))
    }
}

mod graph;
mod mutations;
mod post_chunk;
#[cfg(test)]
mod tests;

pub(super) use graph::*;
