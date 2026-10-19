use super::*;
use crate::services::content::service::{
    RevisionGraphCandidateMaterialization, graph_extract_success_stage_details,
};
use crate::services::graph::error::GraphServiceError;
use crate::services::graph::rebuild::RevisionGraphReconcileOutcome;

fn warn_inline_embedding_failure(
    attempt_id: Uuid,
    revision_id: Uuid,
    error: &crate::services::query::error::QueryServiceError,
) {
    warn!(
        %attempt_id,
        %revision_id,
        ?error,
        "chunk embedding failed; readiness remains count-gated for retry",
    );
}

fn warn_inline_graph_degradation(revision_id: Uuid, reason: &str) {
    warn!(
        %revision_id,
        %reason,
        "inline graph extraction degraded after provider retries; keeping embedded chunk vectors, document remains searchable (graph repair scheduled)",
    );
}

async fn capture_inline_embed_chunk_billing(
    state: &AppState,
    context: &InlineMutationContext,
    attempt_id: Uuid,
    outcome: &crate::services::query::search::EmbedChunksStageOutcome,
) {
    if let Some(command) = CaptureIngestAttemptBillingCommand::for_chunk_embedding(
        context.workspace_id,
        context.library_id,
        attempt_id,
        outcome,
    ) && let Err(error) =
        state.application_services.billing.capture_ingest_attempt(state, command).await
    {
        warn!(%attempt_id, ?error, "embed_chunk billing capture failed; continuing ingest");
    }
}

/// The one terminal `extract_graph` stage marker for the inline path.
///
/// Both outcomes close the *extraction* stage, not the reconcile stage: the
/// candidates were extracted either way, and marking extraction failed when
/// only the projection broke sent operators looking at the wrong stage.
fn graph_reconcile_stage_event(
    attempt_id: Uuid,
    materialization: &RevisionGraphCandidateMaterialization,
    outcome: &Result<RevisionGraphReconcileOutcome, GraphServiceError>,
    graph_ready: bool,
    extract_elapsed_ms: i64,
) -> ingest_repository::NewIngestStageEvent {
    let command = match outcome {
        Ok(outcome) => RecordStageEventCommand::for_graph_materialization(
            attempt_id,
            materialization,
            "completed",
            graph_extract_success_message(graph_ready),
            graph_extract_success_stage_details(materialization, outcome, graph_ready),
            Some(extract_elapsed_ms),
        ),
        Err(error) => RecordStageEventCommand::for_graph_materialization(
            attempt_id,
            materialization,
            "failed",
            format!("graph reconcile failed: {error:#}"),
            serde_json::json!({
                "chunksProcessed": materialization.chunk_count,
                "graphChunksSelected": materialization.selected_graph_chunks,
                "recordStreamSourceUnitsSkipped": materialization.record_stream_source_units_skipped,
                "extractedEntityCandidates": materialization.extracted_entities,
                "extractedRelationCandidates": materialization.extracted_relations,
                "reusedChunks": materialization.reused_chunks,
                "reusedEntities": materialization.reused_entities,
                "reusedRelations": materialization.reused_relations,
                "providerKind": materialization.provider_kind,
                "modelName": materialization.model_name,
            }),
            Some(extract_elapsed_ms),
        ),
    };
    ingest_repository::NewIngestStageEvent::from(command)
}

/// Everything the graph stage of the inline pipeline produced: whether the
/// graph is usable, why it is not, what still needs summarising, the guard
/// that fences publication, and the one stage event that closes
/// `extract_graph`.
struct InlineGraphStage {
    graph_ready: bool,
    graph_failure: Option<String>,
    pending_summary_refresh: Option<crate::services::graph::summary::PendingGraphSummaryRefresh>,
    publication_guard: Option<
        crate::infra::repositories::runtime_graph_repository::IngestRevisionGraphPublicationGuard,
    >,
    terminal_stage_event: ingest_repository::NewIngestStageEvent,
}

/// The identity the inline graph stage runs under, plus the extraction cost
/// every stage event has to report.
///
/// These values travel together through the whole stage, and a separate
/// `attempt_id` is deliberately absent: `authority.attempt_id` already is it,
/// and carrying a second copy is how the stage event and the write fence end up
/// naming different attempts.
#[derive(Clone, Copy)]
struct InlineGraphStageContext<'a> {
    context: &'a InlineMutationContext,
    authority: IngestRevisionWriteAuthority,
    cancellation_token: &'a CancellationToken,
    extract_elapsed_ms: i64,
}

/// Inputs for [`ContentService::run_inline_graph_and_publish`]: the inline
/// attempt's identity plus the embedding fence/profile the publication must
/// carry. Grouped so the publication step reads as one cohesive unit.
struct InlineGraphPublicationInput<'a> {
    context: &'a InlineMutationContext,
    attempt_id: Uuid,
    cancellation_token: &'a CancellationToken,
    vector_write_source_truth_version: i64,
    embed_chunk_profile_key: Option<String>,
}

/// The stage marker for an extraction that never produced candidates.
fn graph_extraction_failure_stage_event(
    attempt_id: Uuid,
    error: &impl std::fmt::Display,
    extract_elapsed_ms: i64,
) -> ingest_repository::NewIngestStageEvent {
    ingest_repository::NewIngestStageEvent::from(RecordStageEventCommand {
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
    })
}

impl ContentService {
    pub(super) async fn run_inline_post_chunk_pipeline(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        attempt_id: Uuid,
        cancellation_token: &CancellationToken,
    ) -> Result<InlineContentPipelineOutcome, InlinePipelineFailure> {
        if cancellation_token.is_cancelled() {
            return Ok(InlineContentPipelineOutcome::AuthorityLost);
        }
        // --- Stage: embed_chunk ------------------------------------------
        // Mirrors the background-ingest worker: embed this revision's
        // chunks synchronously using the library's EmbedChunk binding so
        // the vector lane has queryable rows before we even flip
        // `vector_state` to ready. Prior to this stage the pipeline was
        // a no-op that still promoted vector_state and gave every query
        // a silent zero-vector-hits failure mode.
        state
            .application_services
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
        let vector_write_source_truth_version = repositories::get_library_source_truth_version(
            &state.persistence.postgres,
            context.library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let embed_chunk_start = Instant::now();
        let embed_chunk_result = state
            .application_services
            .search
            .embed_chunks_for_revision(crate::services::query::search::EmbedRevisionChunksInput {
                state,
                library_id: context.library_id,
                revision_id: context.revision_id,
                attempt_id,
                vector_write_source_truth_version,
                cancellation_token,
            })
            .await;
        let embed_chunk_elapsed_ms = Some(embed_chunk_start.elapsed().as_millis() as i64);
        let mut embed_chunk_failure: Option<String> = None;
        match &embed_chunk_result {
            Ok(outcome) => {
                capture_inline_embed_chunk_billing(state, context, attempt_id, outcome).await;
                state
                    .application_services
                    .ingest
                    .record_stage_event(
                        state,
                        RecordStageEventCommand::for_chunk_embedding(
                            attempt_id,
                            outcome,
                            embed_chunk_elapsed_ms,
                        ),
                    )
                    .await?;
                true
            }
            Err(error) => {
                if cancellation_token.is_cancelled() {
                    return Ok(InlineContentPipelineOutcome::AuthorityLost);
                }
                embed_chunk_failure = Some(format!("chunk embedding failed: {error:#}"));
                warn_inline_embedding_failure(attempt_id, context.revision_id, error);
                state
                    .application_services
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
        let embed_chunk_profile_key = embed_chunk_result
            .as_ref()
            .ok()
            .and_then(|outcome| outcome.embedding_profile_key.clone());
        drop(embed_chunk_result);

        if let Some(reason) = embed_chunk_failure {
            // `embed_chunks_for_revision` owns terminal exact-ID cleanup.
            // Repeating a revision-wide delete here could let an expired
            // attempt erase vectors committed by its replacement attempt.
            return Err(InlinePipelineFailure::new(
                ApiError::internal_with_log(&reason, "inline chunk embedding failed"),
                false,
            ));
        }

        // From this point chunk vectors exist. Any later hard failure must
        // request attempt-fenced revision cleanup; otherwise a failed inline
        // mutation could leave unreachable vectors behind. Graph provider or
        // reconcile failures are intentionally converted to a degraded success
        // below and therefore do not enter this cleanup path.
        let post_embed_result = self
            .run_inline_graph_and_publish(
                state,
                InlineGraphPublicationInput {
                    context,
                    attempt_id,
                    cancellation_token,
                    vector_write_source_truth_version,
                    embed_chunk_profile_key,
                },
            )
            .await;
        post_embed_result.map_err(|mut failure| {
            failure.delete_vectors = true;
            failure
        })
    }

    /// Turn the candidate-extraction result into the graph stage of the inline
    /// pipeline.
    ///
    /// `None` means the attempt no longer owns the revision -- whether the
    /// extraction itself reported that, the reconcile did, or the run was
    /// cancelled underneath it. All three end the pipeline the same way, and
    /// keeping them together is what stops one of them from being handled and
    /// the others from silently publishing over another attempt.
    async fn graph_stage_for_materialization(
        &self,
        state: &AppState,
        stage: InlineGraphStageContext<'_>,
        graph_materialization: Result<
            IngestAttemptMutationOutcome<RevisionGraphCandidateMaterialization>,
            crate::services::content::error::ContentServiceError,
        >,
    ) -> Option<InlineGraphStage> {
        match graph_materialization {
            Ok(IngestAttemptMutationOutcome::Applied(graph_materialization)) => {
                self.reconcile_inline_graph(state, stage, graph_materialization).await
            }
            Ok(IngestAttemptMutationOutcome::AuthorityLost) => None,
            Err(_) if stage.cancellation_token.is_cancelled() => None,
            Err(error) => Some(InlineGraphStage {
                graph_ready: false,
                graph_failure: Some(format!("graph candidate extraction failed: {error:#}")),
                pending_summary_refresh: None,
                publication_guard: None,
                terminal_stage_event: graph_extraction_failure_stage_event(
                    stage.authority.attempt_id,
                    &error,
                    stage.extract_elapsed_ms,
                ),
            }),
        }
    }

    /// Reconcile the extracted graph candidates and describe the outcome.
    ///
    /// Returns `None` when the attempt lost authority mid-reconcile — the
    /// caller must abandon the pipeline rather than publish over whoever took
    /// the attempt.
    async fn reconcile_inline_graph(
        &self,
        state: &AppState,
        stage: InlineGraphStageContext<'_>,
        graph_materialization: RevisionGraphCandidateMaterialization,
    ) -> Option<InlineGraphStage> {
        let mut publication_guard = None;
        let reconcile = state
            .application_services
            .graph
            .reconcile_revision_graph(
                state,
                stage.authority,
                stage.context.document_id,
                stage.cancellation_token,
            )
            .await;
        let graph_outcome = match reconcile {
            Ok(IngestAttemptMutationOutcome::Applied(authorized)) => {
                let crate::services::graph::service::AuthorizedRevisionGraphReconcile {
                    outcome,
                    publication_guard: guard,
                } = authorized;
                publication_guard = Some(guard);
                outcome
            }
            Ok(IngestAttemptMutationOutcome::AuthorityLost) => return None,
            Err(error) => Err(error),
        };
        let graph_ready = graph_outcome.as_ref().is_ok_and(|outcome| outcome.graph_ready);
        let terminal_stage_event = graph_reconcile_stage_event(
            stage.authority.attempt_id,
            &graph_materialization,
            &graph_outcome,
            graph_ready,
            stage.extract_elapsed_ms,
        );
        let (pending_summary_refresh, graph_failure) = match graph_outcome {
            Ok(outcome) => (outcome.pending_summary_refresh, None),
            Err(error) => (None, Some(format!("graph reconcile failed: {error:#}"))),
        };
        Some(InlineGraphStage {
            graph_ready,
            graph_failure,
            pending_summary_refresh,
            publication_guard,
            terminal_stage_event,
        })
    }

    async fn run_inline_graph_and_publish(
        &self,
        state: &AppState,
        input: InlineGraphPublicationInput<'_>,
    ) -> Result<InlineContentPipelineOutcome, InlinePipelineFailure> {
        let InlineGraphPublicationInput {
            context,
            attempt_id,
            cancellation_token,
            vector_write_source_truth_version,
            embed_chunk_profile_key,
        } = input;
        state
            .application_services
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
        let revision = state
            .document_store
            .get_revision(context.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_revision", context.revision_id)
            })?;
        if let Some(embedding_profile_key) = embed_chunk_profile_key.as_deref()
            && let Err(error) = ensure_active_embedding_profile_key(
                state,
                context.library_id,
                embedding_profile_key,
            )
            .await
        {
            return Err(InlinePipelineFailure::new(map_runtime_lifecycle_error(error), true));
        }
        let authority = IngestRevisionWriteAuthority {
            workspace_id: context.workspace_id,
            library_id: context.library_id,
            attempt_id,
            revision_id: context.revision_id,
        };
        let extract_start = Instant::now();
        let graph_materialization = self
            .materialize_revision_graph_candidates(
                state,
                MaterializeRevisionGraphCandidatesCommand { authority },
                cancellation_token,
            )
            .await;
        let extract_elapsed_ms = extract_start.elapsed().as_millis() as i64;
        let Some(graph_stage) = self
            .graph_stage_for_materialization(
                state,
                InlineGraphStageContext {
                    context,
                    authority,
                    cancellation_token,
                    extract_elapsed_ms,
                },
                graph_materialization,
            )
            .await
        else {
            return Ok(InlineContentPipelineOutcome::AuthorityLost);
        };
        let InlineGraphStage {
            graph_ready,
            graph_failure,
            pending_summary_refresh,
            publication_guard,
            terminal_stage_event,
        } = graph_stage;
        let terminal_stage_event = Some(terminal_stage_event);

        // The inline path only reaches the graph stage after chunk embedding
        // succeeded (embed failure short-circuits above), so chunk vectors
        // always exist here. Graph is an enrichment layer over chunk-vector
        // retrieval: a terminal graph-extraction failure must not discard those
        // vectors or block the document. Mark the graph layer degraded, keep the
        // vectors, and let the bounded graph recovery scheduler repair the
        // graph later — the document stays searchable via vector + lexical retrieval.
        let graph_degraded = if let Some(reason) = graph_failure {
            warn_inline_graph_degradation(context.revision_id, &reason);
            true
        } else {
            false
        };

        let now = Utc::now();
        let graph_state = if graph_degraded {
            super::GRAPH_STATE_DEGRADED
        } else {
            graph_state_after_successful_extract(graph_ready)
        };
        let publication_input = ingest_repository::PublishContentIngestSuccess {
            workspace_id: context.workspace_id,
            library_id: context.library_id,
            document_id: context.document_id,
            revision_id: context.revision_id,
            mutation_id: context.mutation_id,
            mutation_item_id: context.item_id,
            attempt_id,
            expected_source_truth_version: vector_write_source_truth_version,
            embedding_profile_key: embed_chunk_profile_key,
            text_state: revision.text_state,
            graph_state: graph_state.to_string(),
            text_readable_at: revision.text_readable_at,
            graph_ready_at: revision.graph_ready_at.or_else(|| graph_ready.then_some(now)),
            completed_at: now,
        };
        let effects = ingest_repository::ContentIngestPublicationEffects {
            document_summary: None,
            terminal_stage_events: terminal_stage_event.into_iter().collect(),
        };
        if cancellation_token.is_cancelled() {
            if let Some(publication_guard) = publication_guard {
                publication_guard.rollback().await.map_err(|error| {
                    InlinePipelineFailure::new(
                        ApiError::internal_with_log(
                            error,
                            "failed to roll back canceled inline graph reconcile",
                        ),
                        true,
                    )
                })?;
            }
            return Ok(InlineContentPipelineOutcome::AuthorityLost);
        }
        let publication = (if let Some(publication_guard) = publication_guard {
            ingest_repository::publish_content_ingest_success_revision_fenced_with_graph_guard(
                publication_guard,
                &publication_input,
                &effects,
            )
            .await
        } else {
            ingest_repository::publish_content_ingest_success_revision_fenced_with_effects(
                &state.persistence.postgres,
                &publication_input,
                &effects,
                state,
            )
            .await
        })
        .map_err(|error| {
            InlinePipelineFailure::new(
                ApiError::internal_with_log(error, "failed to publish inline content ingest"),
                true,
            )
        })?;

        Ok(match publication {
            ingest_repository::PublishContentIngestSuccessOutcome::Applied {
                source_truth_version,
                ..
            } => {
                self.refresh_inline_published_graph_summary(
                    state,
                    context,
                    source_truth_version,
                    pending_summary_refresh,
                )
                .await;
                InlineContentPipelineOutcome::Applied
            }
            ingest_repository::PublishContentIngestSuccessOutcome::AuthorityLost { .. } => {
                InlineContentPipelineOutcome::AuthorityLost
            }
        })
    }

    async fn refresh_inline_published_graph_summary(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
        source_truth_version: i64,
        pending_summary_refresh: Option<
            crate::services::graph::summary::PendingGraphSummaryRefresh,
        >,
    ) {
        if let Some(pending_summary_refresh) = pending_summary_refresh
            && let Err(error) = state
                .application_services
                .graph
                .apply_published_summary_refresh(
                    state,
                    context.library_id,
                    source_truth_version,
                    &pending_summary_refresh,
                )
                .await
        {
            warn!(
                library_id = %context.library_id,
                revision_id = %context.revision_id,
                source_truth_version,
                ?error,
                "inline content lifecycle committed but active graph summary refresh failed",
            );
        }
    }
}
