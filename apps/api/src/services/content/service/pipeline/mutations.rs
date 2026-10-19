use super::*;
use crate::domains::ops::OpsFailureDisposition;
use futures::FutureExt as _;

use crate::services::content::service::{preparation_fence_text, sha256_hex_text};

/// Marks a leased inline attempt failed when the detached pipeline exits
/// abnormally — a panic, or an abort while the runtime shuts down — without
/// reaching its own finalizer. The caller may already be gone, so nothing else
/// observes the failure; without this the attempt would stay `leased` until the
/// stale-lease reaper requeued and redid the whole ingest.
///
/// `finalize_failed_inline_mutation` is async, so the drop spawns a best-effort
/// detached finalizer. If no runtime is available the attempt is left to the
/// reaper, which is the documented last resort.
struct InlineAttemptAbortGuard {
    service: ContentService,
    state: AppState,
    context: InlineMutationContext,
    attempt_id: Uuid,
    attempt_started_at: chrono::DateTime<Utc>,
    armed: bool,
}

impl InlineAttemptAbortGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InlineAttemptAbortGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                attempt_id = %self.attempt_id,
                "inline ingest aborted with no runtime to finalize its attempt; the stale-lease reaper will recover it",
            );
            return;
        };
        let service = self.service.clone();
        let state = self.state.clone();
        let context = self.context.clone();
        let attempt_id = self.attempt_id;
        let attempt_started_at = self.attempt_started_at;
        runtime.spawn(async move {
            let failure = InlinePipelineFailure::new(ApiError::Internal, false);
            service
                .finalize_failed_inline_mutation(
                    &state,
                    InlineFailureFinalization {
                        context: &context,
                        attempt_id,
                        attempt_started_at,
                        failure: &failure,
                        failure_code: "inline_materialization_aborted",
                    },
                )
                .await;
        });
    }
}

/// The identity, timing and failure payload of an inline attempt that must be
/// published as terminal. Grouped so the finalizer reads as one cohesive unit.
struct InlineFailureFinalization<'a> {
    context: &'a InlineMutationContext,
    attempt_id: Uuid,
    attempt_started_at: chrono::DateTime<Utc>,
    failure: &'a InlinePipelineFailure,
    failure_code: &'a str,
}

struct InlineTextSource<'a> {
    text: &'a str,
    file_name: Option<&'a str>,
    mime_type: Option<&'a str>,
}

impl ContentService {
    /// Materializes an inline append. Admission has already committed in the
    /// caller; the lease, heartbeat and whole pipeline run in a detached
    /// [`tokio::spawn`] task whose handle is awaited here.
    ///
    /// Awaiting the handle keeps the handler's reply shape and surfaces a
    /// panic as a failed admission. But when the caller future is dropped — a
    /// client or proxy abandoning the request — the awaited handle is dropped
    /// too, and tokio **detaches** rather than aborts the spawned task. The
    /// ingest therefore keeps heartbeating and reaches a terminal state
    /// normally. Before this seam the pipeline ran inside the request future:
    /// a disconnect killed it mid-`extract_graph`, the heartbeat guard dropped
    /// silently, the attempt stayed `leased`, and the 60s stale-lease reaper
    /// requeued the job and redid the whole ingest.
    ///
    /// This one helper is the seam for every inline append caller (the REST
    /// revision handler and the MCP mutation surface both route through
    /// [`ContentService::append_inline_mutation`]). `edit` and `replace` need
    /// no equivalent: they only admit an async job and return, and the worker
    /// materializes them.
    pub(super) async fn materialize_inline_text_mutation(
        &self,
        state: &AppState,
        admission: &ContentMutationAdmission,
        text: String,
        file_name: Option<String>,
        mime_type: Option<String>,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let mutation_id = admission.mutation.id;
        let service = self.clone();
        let state = state.clone();
        let admission = admission.clone();
        // A spawned task does not inherit the ambient span, so attach the
        // request span explicitly: an ingest detached by a client disconnect
        // must stay correlatable with the request that admitted it.
        let request_span = tracing::Span::current();
        let ingest = tokio::spawn(tracing::Instrument::instrument(
            async move {
                // `Box::pin` keeps the whole inline pipeline future off this
                // task's await stack; `clippy::large-futures` caps it at 20 KiB.
                Box::pin(
                    service.run_inline_text_mutation(state, admission, text, file_name, mime_type),
                )
                .await
            },
            request_span,
        ));
        match ingest.await {
            Ok(result) => result,
            Err(join_error) => {
                // `tokio::spawn` yields a `JoinError` only when the task
                // panicked or was explicitly aborted, never when this future is
                // dropped. Never swallow it: report the failure. A panic is
                // already finalized in-task (and resumed); an abort mid-runtime
                // shutdown is finalized by the guard's detached finalizer or,
                // failing that, recovered by the stale-lease reaper.
                tracing::error!(
                    %mutation_id,
                    error = %join_error,
                    "detached inline ingest task did not join; its attempt will be reconciled by the stale-lease reaper",
                );
                Err(ApiError::internal_with_log(
                    join_error,
                    "detached inline ingest task join failed",
                ))
            }
        }
    }

    async fn run_inline_text_mutation(
        &self,
        state: AppState,
        admission: ContentMutationAdmission,
        text: String,
        file_name: Option<String>,
        mime_type: Option<String>,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let state = &state;
        let admission = &admission;
        let context = self.inline_mutation_context_from_admission(admission)?;
        let attempt = self.lease_inline_attempt(state, &context).await?;
        let cancellation_token = CancellationToken::new();
        let heartbeat_guard =
            spawn_inline_attempt_heartbeat(state, attempt.id, cancellation_token.clone());
        let authority = IngestRevisionWriteAuthority {
            workspace_id: context.workspace_id,
            library_id: context.library_id,
            attempt_id: attempt.id,
            revision_id: context.revision_id,
        };
        // Armed after the lease. If this task exits abnormally without reaching
        // the finalizer below, the guard marks the attempt failed; the caller
        // may already be detached and unable to observe the panic.
        let mut abort_guard = InlineAttemptAbortGuard {
            service: self.clone(),
            state: state.clone(),
            context: context.clone(),
            attempt_id: attempt.id,
            attempt_started_at: attempt.started_at,
            armed: true,
        };
        // Keep every post-lease fallible operation inside one lifecycle
        // boundary. Any error before publication is settled by the exact-item
        // failure UoW; work after a committed publication is deliberately kept
        // outside this boundary and can never rewrite the terminal state.
        //
        // `catch_unwind` turns a panic inside the detached pipeline into a
        // terminal failure instead of an unwind that skips the finalizer; the
        // panic is resumed after finalization so it still surfaces in the task
        // result.
        let lifecycle_result = std::panic::AssertUnwindSafe(async {
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
            self.run_inline_pre_publication_stages(
                state,
                &context,
                authority,
                InlineTextSource {
                    text: &text,
                    file_name: file_name.as_deref(),
                    mime_type: mime_type.as_deref(),
                },
                &cancellation_token,
            )
            .await
        })
        .catch_unwind()
        .await;
        drop(heartbeat_guard);

        match lifecycle_result {
            Ok(Ok(InlineContentPipelineOutcome::Applied)) => {
                abort_guard.disarm();
                // This inline path publishes the attempt terminal state itself
                // and never calls `finalize_attempt`, so the success metric is
                // recorded here (IRG-204).
                self.finish_inline_applied_mutation(state, &context, attempt.started_at).await
            }
            Ok(Ok(InlineContentPipelineOutcome::AuthorityLost)) => {
                abort_guard.disarm();
                warn!(
                    attempt_id = %attempt.id,
                    mutation_id = %context.mutation_id,
                    "inline content publication arrived after attempt authority moved; preserving current owner state",
                );
                self.get_mutation_admission(state, context.mutation_id).await
            }
            Ok(Err(failure)) => {
                abort_guard.disarm();
                self.finalize_failed_inline_mutation(
                    state,
                    InlineFailureFinalization {
                        context: &context,
                        attempt_id: attempt.id,
                        attempt_started_at: attempt.started_at,
                        failure: &failure,
                        failure_code: "inline_pipeline_failed",
                    },
                )
                .await;
                Err(failure.error)
            }
            Err(payload) => {
                // Finalize before resuming the detached task's panic.
                abort_guard.disarm();
                let failure = InlinePipelineFailure::new(ApiError::Internal, false);
                self.finalize_failed_inline_mutation(
                    state,
                    InlineFailureFinalization {
                        context: &context,
                        attempt_id: attempt.id,
                        attempt_started_at: attempt.started_at,
                        failure: &failure,
                        failure_code: "inline_materialization_panicked",
                    },
                )
                .await;
                std::panic::resume_unwind(payload);
            }
        }
    }

    async fn finish_inline_applied_mutation(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_started_at: chrono::DateTime<Utc>,
    ) -> Result<ContentMutationAdmission, ApiError> {
        crate::observability::metrics::record_ingest_attempt(
            "content_mutation",
            crate::observability::metrics::IngestOutcome::Succeeded,
            None,
            crate::observability::metrics::elapsed_seconds(attempt_started_at, Utc::now()),
        );
        invalidate_library_embedding_profile_inventory(context.library_id);
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
                "post-publication technical fact convergence failed after inline mutation commit"
            );
        }
        self.get_mutation_admission(state, context.mutation_id).await
    }

    /// Runs the pre-publication inline stages (extract, prepare structure, chunk,
    /// technical facts) and then the post-chunk pipeline. Kept as one method so
    /// the unwinding lifecycle boundary in [`Self::run_inline_text_mutation`]
    /// stays small; the stage order and every event are unchanged.
    async fn run_inline_pre_publication_stages(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        authority: IngestRevisionWriteAuthority,
        source: InlineTextSource<'_>,
        cancellation_token: &CancellationToken,
    ) -> Result<InlineContentPipelineOutcome, InlinePipelineFailure> {
        let InlineTextSource { text, file_name, mime_type } = source;
        let attempt_id = authority.attempt_id;
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_EXTRACT_CONTENT,
                    "started",
                    Some("materializing appended text".to_string()),
                    serde_json::json!({
                        "documentId": context.document_id,
                        "revisionId": context.revision_id,
                    }),
                    None,
                ),
            )
            .await?;
        let stage_start = Instant::now();
        // Build the extraction plan before persisting the extract artifact:
        // the revision text and its checksum must be the exact text
        // `prepare_and_persist_revision_structure` hashes for the fence.
        let extraction_plan = if file_name.is_some() || mime_type.is_some() {
            build_inline_text_extraction_plan_for_source(text, file_name, mime_type).map_err(
                |error| ApiError::BadRequest(format!("inline extraction failed: {error}")),
            )?
        } else {
            build_inline_text_extraction_plan(text)
        };
        let extracted_text = preparation_fence_text(&extraction_plan);
        let extract_outcome = state
            .application_services
            .knowledge
            .set_ingest_revision_extract_state(
                state,
                authority,
                crate::services::knowledge::service::IngestRevisionExtractContent {
                    extract_state: "ready",
                    normalized_text: Some(&extracted_text),
                    text_checksum: Some(&sha256_hex_text(&extracted_text)),
                    image_checksum: None,
                },
            )
            .await?;
        if matches!(extract_outcome, IngestAttemptMutationOutcome::AuthorityLost) {
            return Ok(InlineContentPipelineOutcome::AuthorityLost);
        }
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_EXTRACT_CONTENT,
                    "completed",
                    Some("appended text materialized".to_string()),
                    serde_json::json!({ "contentLength": extracted_text.chars().count() }),
                    Some(stage_start.elapsed().as_millis() as i64),
                ),
            )
            .await?;
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_PREPARE_STRUCTURE,
                    "started",
                    Some("building structured revision from normalized text".to_string()),
                    serde_json::json!({ "revisionId": context.revision_id }),
                    None,
                ),
            )
            .await?;
        let preparation_outcome = match self
            .prepare_and_persist_revision_structure(
                state,
                authority,
                &extraction_plan,
                cancellation_token,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(_) if cancellation_token.is_cancelled() => {
                return Ok(InlineContentPipelineOutcome::AuthorityLost);
            }
            Err(error) => return Err(ApiError::from(error).into()),
        };
        let preparation = match preparation_outcome {
            IngestAttemptMutationOutcome::Applied(preparation) => preparation,
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Ok(InlineContentPipelineOutcome::AuthorityLost);
            }
        };
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_PREPARE_STRUCTURE,
                    "completed",
                    Some("structured revision prepared".to_string()),
                    serde_json::json!({
                        "revisionId": context.revision_id,
                        "normalizationProfile": preparation.normalization_profile,
                        "blockCount": preparation.prepared_revision.block_count,
                        "chunkCount": preparation.chunk_count,
                    }),
                    Some(preparation.prepare_structure_elapsed_ms),
                ),
            )
            .await?;
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_CHUNK_CONTENT,
                    "started",
                    Some("persisting content chunks".to_string()),
                    serde_json::json!({
                        "revisionId": context.revision_id,
                    }),
                    None,
                ),
            )
            .await?;
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_CHUNK_CONTENT,
                    "completed",
                    Some("content chunks persisted".to_string()),
                    serde_json::json!({
                        "revisionId": context.revision_id,
                        "chunkCount": preparation.chunk_count,
                    }),
                    Some(preparation.chunk_content_elapsed_ms),
                ),
            )
            .await?;
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
                    "started",
                    Some("extracting technical facts from structured revision".to_string()),
                    serde_json::json!({
                        "revisionId": context.revision_id,
                    }),
                    None,
                ),
            )
            .await?;
        state
            .application_services
            .ingest
            .record_stage_event(
                state,
                RecordStageEventCommand::for_stage(
                    attempt_id,
                    INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
                    "completed",
                    Some("technical facts extracted from structured revision".to_string()),
                    serde_json::json!({
                        "revisionId": context.revision_id,
                        "technicalFactCount": preparation.technical_fact_count,
                        "technicalConflictCount": preparation.technical_conflict_count,
                    }),
                    Some(preparation.extract_technical_facts_elapsed_ms),
                ),
            )
            .await?;
        self.run_inline_post_chunk_pipeline(state, context, attempt_id, cancellation_token).await
    }

    async fn finalize_failed_inline_mutation(
        &self,
        state: &AppState,
        input: InlineFailureFinalization<'_>,
    ) {
        let InlineFailureFinalization {
            context,
            attempt_id,
            attempt_started_at,
            failure,
            failure_code,
        } = input;
        let failure_message = format!("{failure_code}: {}", failure.error);
        match ingest_repository::fail_content_ingest_attempt(
            &state.persistence.postgres,
            &ingest_repository::FailContentIngestAttempt {
                workspace_id: context.workspace_id,
                library_id: context.library_id,
                document_id: context.document_id,
                revision_id: context.revision_id,
                mutation_id: context.mutation_id,
                mutation_item_id: context.item_id,
                attempt_id,
                current_stage: None,
                failure_class: Some("content_mutation".to_string()),
                failure_code: Some(failure_code.to_string()),
                // An inline pipeline fault is a terminal operational failure,
                // not a rejected content admission.
                failure_disposition: Some(OpsFailureDisposition::OperationalTerminal),
                failure_message: Some(failure_message),
                retryable: false,
                operational_defer: None,
                delete_vectors: failure.delete_vectors,
                failed_at: Utc::now(),
            },
        )
        .await
        {
            Ok(ingest_repository::FailContentIngestAttemptOutcome::Applied { .. }) => {
                invalidate_library_embedding_profile_inventory(context.library_id);
                // The attempt is now terminal in the repository but never
                // passed through `finalize_attempt`; record its failure and
                // duration here (IRG-204).
                crate::observability::metrics::record_ingest_attempt(
                    "content_mutation",
                    crate::observability::metrics::IngestOutcome::Failed,
                    Some(failure_code),
                    crate::observability::metrics::elapsed_seconds(attempt_started_at, Utc::now()),
                );
            }
            Ok(ingest_repository::FailContentIngestAttemptOutcome::AuthorityLost { .. }) => {
                warn!(
                    mutation_id = %context.mutation_id,
                    attempt_id = %attempt_id,
                    "inline failure arrived after attempt authority moved; preserving current owner state",
                );
            }
            Err(finalize_error) => {
                warn_inline_failure_finalization(context.mutation_id, attempt_id, finalize_error);
            }
        }
    }
}

fn warn_inline_failure_finalization(
    mutation_id: Uuid,
    attempt_id: Uuid,
    finalize_error: impl std::fmt::Debug,
) {
    warn!(
        %mutation_id,
        %attempt_id,
        ?finalize_error,
        "failed to atomically publish inline mutation failure; lease recovery will reconcile the attempt",
    );
}
