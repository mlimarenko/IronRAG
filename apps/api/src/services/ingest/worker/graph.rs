use std::time::Instant;

use anyhow::Context;
use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use crate::{
    infra::{
        knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
        repositories::{self, ingest_repository},
    },
    services::{
        content::service::{
            GRAPH_STATE_DEGRADED, MaterializeRevisionGraphCandidatesCommand,
            RevisionGraphCandidateMaterialization, graph_extract_success_message,
            graph_extract_success_stage_details, graph_state_after_successful_extract,
        },
        ingest::service::{INGEST_STAGE_EXTRACT_GRAPH, RecordStageEventCommand},
    },
};

use super::attempt::IngestStageContext;
use super::errors::{JobLeaseLost, is_provider_lane_acquisition_failure};

pub(super) enum GraphRefreshOutcome {
    LifecyclePublished,
}

pub(super) struct GraphExtractionOutcome {
    pub(super) graph_ready: bool,
    pub(super) graph_degraded: bool,
    pub(super) pending_summary_refresh:
        Option<crate::services::graph::summary::PendingGraphSummaryRefresh>,
    pub(super) publication_guard:
        Option<repositories::runtime_graph_repository::IngestRevisionGraphPublicationGuard>,
    pub(super) terminal_stage_event: Option<ingest_repository::NewIngestStageEvent>,
}

impl RecordStageEventCommand {
    pub(crate) fn for_graph_materialization(
        attempt_id: Uuid,
        materialization: &RevisionGraphCandidateMaterialization,
        stage_state: &str,
        message: impl Into<String>,
        details_json: serde_json::Value,
        elapsed_ms: Option<i64>,
    ) -> Self {
        Self {
            attempt_id,
            stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
            stage_state: stage_state.to_string(),
            message: Some(message.into()),
            details_json,
            provider_kind: materialization.provider_kind.clone(),
            model_name: materialization.model_name.clone(),
            prompt_tokens: graph_usage_token_count(&materialization.usage_json, "prompt_tokens"),
            completion_tokens: graph_usage_token_count(
                &materialization.usage_json,
                "completion_tokens",
            ),
            total_tokens: graph_usage_token_count(&materialization.usage_json, "total_tokens"),
            cached_tokens: None,
            estimated_cost: None,
            currency_code: None,
            elapsed_ms,
        }
    }
}

impl From<RecordStageEventCommand> for ingest_repository::NewIngestStageEvent {
    fn from(command: RecordStageEventCommand) -> Self {
        Self {
            attempt_id: command.attempt_id,
            stage_name: command.stage_name,
            stage_state: command.stage_state,
            ordinal: 0,
            message: command.message,
            details_json: command.details_json,
            recorded_at: None,
            provider_kind: command.provider_kind,
            model_name: command.model_name,
            prompt_tokens: command.prompt_tokens,
            completion_tokens: command.completion_tokens,
            total_tokens: command.total_tokens,
            cached_tokens: command.cached_tokens,
            estimated_cost: command.estimated_cost,
            currency_code: command.currency_code,
            elapsed_ms: command.elapsed_ms,
        }
    }
}

fn graph_usage_token_count(usage_json: &serde_json::Value, field: &str) -> Option<i32> {
    usage_json
        .get(field)
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

pub(super) async fn run_graph_refresh(
    context: IngestStageContext<'_>,
) -> anyhow::Result<GraphRefreshOutcome> {
    let IngestStageContext {
        state, job, attempt_id, document_id, revision_id, cancellation, ..
    } = context;
    let cancellation_token = cancellation.token;
    anyhow::ensure!(
        job.job_kind == "graph_refresh",
        "graph-refresh runner received another job kind"
    );
    anyhow::ensure!(
        job.mutation_id.is_none()
            && job.mutation_item_id.is_none()
            && job.async_operation_id.is_none(),
        "graph-refresh maintenance must not own a content mutation or async operation"
    );
    cancellation.check(job.id)?;

    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                stage_state: "started".to_string(),
                message: Some("refreshing graph candidates for the current revision".to_string()),
                details_json: serde_json::json!({
                    "documentId": document_id,
                    "revisionId": revision_id,
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
        .await
        .context("failed to record graph-refresh start stage event")?;

    let started_at = Instant::now();
    let authority = IngestRevisionWriteAuthority {
        workspace_id: job.workspace_id,
        library_id: job.library_id,
        attempt_id,
        revision_id,
    };
    let materialization = match state
        .application_services
        .content
        .materialize_revision_graph_candidates(
            state,
            MaterializeRevisionGraphCandidatesCommand { authority },
            cancellation_token,
        )
        .await
    {
        Ok(IngestAttemptMutationOutcome::Applied(materialization)) => materialization,
        Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
            return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
        }
        Err(crate::services::content::error::ContentServiceError::Cancelled) => {
            return Err(cancellation.error(job.id));
        }
        Err(error) => return Err(error).context("graph-refresh candidate extraction failed"),
    };

    let authorized_reconcile = match state
        .application_services
        .graph
        .reconcile_revision_graph(state, authority, document_id, cancellation_token)
        .await
    {
        Ok(IngestAttemptMutationOutcome::Applied(reconcile)) => reconcile,
        Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
            return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
        }
        Err(crate::services::graph::error::GraphServiceError::RevisionSuperseded { .. }) => {
            return finalize_superseded_graph_refresh(&context, &materialization, started_at).await;
        }
        Err(crate::services::graph::error::GraphServiceError::Cancelled) => {
            return Err(cancellation.error(job.id));
        }
        Err(error) => return Err(error).context("graph-refresh reconcile admission failed"),
    };
    let crate::services::graph::service::AuthorizedRevisionGraphReconcile {
        outcome: graph_outcome,
        publication_guard,
    } = authorized_reconcile;
    let (graph_ready, graph_degraded, pending_summary_refresh, terminal_event) = match graph_outcome
    {
        Ok(outcome) => {
            let graph_ready = outcome.graph_ready;
            let event =
                completed_graph_stage_event(attempt_id, &materialization, &outcome, started_at);
            (graph_ready, false, outcome.pending_summary_refresh, event)
        }
        Err(error) => {
            log_graph_refresh_degraded(attempt_id, revision_id, &error);
            let event = ingest_repository::NewIngestStageEvent::from(
                RecordStageEventCommand::for_graph_materialization(
                    attempt_id,
                    &materialization,
                    "failed",
                    "graph reconcile failed",
                    serde_json::json!({
                        "graphReady": false,
                        "error": error.to_string(),
                    }),
                    Some(started_at.elapsed().as_millis() as i64),
                ),
            );
            (false, true, None, event)
        }
    };

    if cancellation.token.is_cancelled()
        || cancellation.user_requested.load(std::sync::atomic::Ordering::Acquire)
        || cancellation.operator_pause_requested.load(std::sync::atomic::Ordering::Acquire)
        || cancellation.lease_lost_requested.load(std::sync::atomic::Ordering::Acquire)
    {
        publication_guard.rollback().await.context("roll back canceled graph-refresh reconcile")?;
        return Err(cancellation.error(job.id));
    }

    let completed_at = Utc::now();
    let publication = ingest_repository::publish_graph_refresh_success_with_graph_guard(
        publication_guard,
        &ingest_repository::PublishGraphRefreshSuccess {
            workspace_id: job.workspace_id,
            library_id: job.library_id,
            document_id,
            revision_id,
            attempt_id,
            graph_state: if graph_degraded {
                GRAPH_STATE_DEGRADED.to_string()
            } else {
                graph_state_after_successful_extract(graph_ready).to_string()
            },
            graph_ready_at: graph_ready.then_some(completed_at),
            completed_at,
        },
        &ingest_repository::GraphRefreshPublicationEffects {
            terminal_stage_events: vec![terminal_event],
        },
    )
    .await
    .context("failed to atomically publish graph-refresh success")?;

    match publication {
        ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { source_truth_version } => {
            refresh_published_graph_summary(
                &context,
                source_truth_version,
                pending_summary_refresh.as_ref(),
            )
            .await;
            log_graph_refresh_published(&context);
        }
        ingest_repository::PublishGraphRefreshSuccessOutcome::Superseded { .. } => {
            log_graph_refresh_superseded(&context);
        }
        ingest_repository::PublishGraphRefreshSuccessOutcome::AuthorityLost { .. } => {
            log_graph_refresh_authority_loss(&context);
        }
    }

    Ok(GraphRefreshOutcome::LifecyclePublished)
}

pub(super) async fn extract_revision_graph(
    context: &IngestStageContext<'_>,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
) -> anyhow::Result<GraphExtractionOutcome> {
    let state = context.state;
    let job = context.job;
    let attempt_id = context.attempt_id;
    let document_id = context.document_id;
    let revision_id = context.revision_id;
    let cancellation = &context.cancellation;
    // --- Stage: extract_graph -------------------------------------------------
    cancellation.check(job.id)?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand::for_stage(
                attempt_id,
                INGEST_STAGE_EXTRACT_GRAPH,
                "started",
                Some("extracting graph candidates from chunks".to_string()),
                serde_json::json!({
                    "libraryId": revision.library_id,
                    "revisionId": revision_id,
                }),
                None,
            ),
        )
        .await
        .context("failed to record extract_graph start stage event")?;

    let extract_graph_start = Instant::now();
    let authority = IngestRevisionWriteAuthority {
        workspace_id: revision.workspace_id,
        library_id: revision.library_id,
        attempt_id,
        revision_id,
    };
    let graph_materialization = match state
        .application_services
        .content
        .materialize_revision_graph_candidates(
            state,
            MaterializeRevisionGraphCandidatesCommand { authority },
            cancellation.token,
        )
        .await
    {
        Ok(IngestAttemptMutationOutcome::Applied(materialization)) => Ok(materialization),
        Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
            return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
        }
        Err(crate::services::content::error::ContentServiceError::Cancelled) => {
            return Err(cancellation.error(job.id));
        }
        Err(error) if is_provider_lane_acquisition_failure(&error) => {
            return Err(anyhow::Error::new(error)
                .context("provider concurrency admission interrupted graph extraction"));
        }
        Err(error) => Err(error),
    };
    let mut graph_ready = false;
    let mut graph_failure: Option<String> = None;
    let mut pending_summary_refresh = None;
    let mut publication_guard = None;
    let terminal_graph_stage_event;

    match graph_materialization {
        Ok(graph_materialization) => {
            let graph_outcome = match state
                .application_services
                .graph
                .reconcile_revision_graph(state, authority, document_id, cancellation.token)
                .await
            {
                Ok(IngestAttemptMutationOutcome::Applied(authorized)) => {
                    let crate::services::graph::service::AuthorizedRevisionGraphReconcile {
                        outcome,
                        publication_guard: guard,
                    } = authorized;
                    publication_guard = Some(guard);
                    outcome
                }
                Ok(IngestAttemptMutationOutcome::AuthorityLost) => {
                    return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
                }
                Err(crate::services::graph::error::GraphServiceError::Cancelled) => {
                    return Err(cancellation.error(job.id));
                }
                Err(error) => Err(error),
            };
            graph_ready = graph_outcome.as_ref().is_ok_and(|outcome| outcome.graph_ready);

            match graph_outcome {
                Ok(outcome) => {
                    pending_summary_refresh = outcome.pending_summary_refresh.clone();
                    let extract_graph_elapsed_ms =
                        Some(extract_graph_start.elapsed().as_millis() as i64);
                    terminal_graph_stage_event =
                        Some(ingest_repository::NewIngestStageEvent::from(
                            RecordStageEventCommand::for_graph_materialization(
                                attempt_id,
                                &graph_materialization,
                                "completed",
                                graph_extract_success_message(graph_ready),
                                graph_extract_success_stage_details(
                                    &graph_materialization,
                                    &outcome,
                                    graph_ready,
                                ),
                                extract_graph_elapsed_ms,
                            ),
                        ));
                }
                Err(graph_error) => {
                    graph_failure = Some(format!("graph reconcile failed: {graph_error:#}"));
                    log_graph_rebuild_failure(context, &graph_error);
                    let extract_graph_elapsed_ms =
                        Some(extract_graph_start.elapsed().as_millis() as i64);
                    terminal_graph_stage_event = Some(
                        ingest_repository::NewIngestStageEvent::from(
                            RecordStageEventCommand::for_graph_materialization(
                                attempt_id,
                                &graph_materialization,
                                "failed",
                                "graph rebuild failed",
                                serde_json::json!({
                                    "chunksProcessed": graph_materialization.chunk_count,
                                    "graphChunksSelected": graph_materialization.selected_graph_chunks,
                                    "recordStreamSourceUnitsSkipped": graph_materialization.record_stream_source_units_skipped,
                                    "extractedEntityCandidates": graph_materialization.extracted_entities,
                                    "extractedRelationCandidates": graph_materialization.extracted_relations,
                                    "graphReady": false,
                                    "error": format!("{graph_error:#}"),
                                    "providerKind": graph_materialization.provider_kind,
                                    "modelName": graph_materialization.model_name,
                                }),
                                extract_graph_elapsed_ms,
                            ),
                        ),
                    );
                }
            }
        }
        Err(error) => {
            graph_failure = Some(format!("graph candidate extraction failed: {error:#}"));
            log_graph_candidate_failure(context, &error);
            let extract_graph_elapsed_ms = Some(extract_graph_start.elapsed().as_millis() as i64);
            terminal_graph_stage_event =
                Some(ingest_repository::NewIngestStageEvent::from(RecordStageEventCommand {
                    attempt_id,
                    stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                    stage_state: "failed".to_string(),
                    message: Some("graph candidate extraction failed".to_string()),
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
                    elapsed_ms: extract_graph_elapsed_ms,
                }));
        }
    }

    // Graph extraction is an enrichment layer over chunk-vector retrieval, not
    // the core. When the chunks for this revision are already embedded, a
    // terminal graph-extraction failure must NOT discard those vectors or block
    // the document from becoming searchable. Mark the graph layer degraded, keep
    // the vectors, promote the document as readable, and let the bounded graph
    // recovery scheduler repair the graph on a later tick. Only when the vectors
    // themselves are missing do we fail destructively (nothing to preserve).
    let graph_degraded = graph_failure.is_some();
    if let Some(reason) = graph_failure {
        log_graph_extraction_degraded(context, &reason);
    }
    Ok(GraphExtractionOutcome {
        graph_ready,
        graph_degraded,
        pending_summary_refresh,
        publication_guard,
        terminal_stage_event: terminal_graph_stage_event,
    })
}

async fn finalize_superseded_graph_refresh(
    context: &IngestStageContext<'_>,
    materialization: &RevisionGraphCandidateMaterialization,
    started_at: Instant,
) -> anyhow::Result<GraphRefreshOutcome> {
    let IngestStageContext { state, job, document_id, revision_id, attempt_id, .. } = *context;
    let completed_at = Utc::now();
    let terminal_event = ingest_repository::NewIngestStageEvent::from(
        RecordStageEventCommand::for_graph_materialization(
            attempt_id,
            materialization,
            "failed",
            "graph refresh revision was superseded",
            serde_json::json!({ "graphReady": false, "revisionId": revision_id, "reason": "revision_superseded" }),
            Some(started_at.elapsed().as_millis() as i64),
        ),
    );
    let publication = ingest_repository::publish_graph_refresh_success_with_effects(
        &state.persistence.postgres,
        &ingest_repository::PublishGraphRefreshSuccess {
            workspace_id: job.workspace_id,
            library_id: job.library_id,
            document_id,
            revision_id,
            attempt_id,
            graph_state: GRAPH_STATE_DEGRADED.to_string(),
            graph_ready_at: None,
            completed_at,
        },
        &ingest_repository::GraphRefreshPublicationEffects {
            terminal_stage_events: vec![terminal_event],
        },
    )
    .await
    .context("finalize superseded graph-refresh job")?;
    match publication {
        ingest_repository::PublishGraphRefreshSuccessOutcome::Superseded { .. } => {
            Ok(GraphRefreshOutcome::LifecyclePublished)
        }
        ingest_repository::PublishGraphRefreshSuccessOutcome::AuthorityLost { .. } => {
            Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }))
        }
        ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { .. } => {
            Err(anyhow::anyhow!("superseded graph refresh unexpectedly became current"))
        }
    }
}

fn completed_graph_stage_event(
    attempt_id: Uuid,
    materialization: &RevisionGraphCandidateMaterialization,
    outcome: &crate::services::graph::rebuild::RevisionGraphReconcileOutcome,
    started_at: Instant,
) -> ingest_repository::NewIngestStageEvent {
    let mut command = RecordStageEventCommand::for_graph_materialization(
        attempt_id,
        materialization,
        "completed",
        graph_extract_success_message(outcome.graph_ready),
        graph_extract_success_stage_details(materialization, outcome, outcome.graph_ready),
        None,
    );
    command.elapsed_ms = Some(started_at.elapsed().as_millis() as i64);
    command.into()
}

fn log_graph_refresh_degraded(attempt_id: Uuid, revision_id: Uuid, error: &impl std::fmt::Debug) {
    warn!(%attempt_id, %revision_id, ?error, "graph-refresh reconcile degraded");
}

async fn refresh_published_graph_summary(
    context: &IngestStageContext<'_>,
    source_truth_version: i64,
    pending_summary_refresh: Option<&crate::services::graph::summary::PendingGraphSummaryRefresh>,
) {
    if let Some(pending_summary_refresh) = pending_summary_refresh
        && let Err(error) = context
            .state
            .application_services
            .graph
            .apply_published_summary_refresh(
                context.state,
                context.job.library_id,
                source_truth_version,
                pending_summary_refresh,
            )
            .await
    {
        warn!(worker_id = %context.worker_id, job_id = %context.job.id,
            attempt_id = %context.attempt_id, revision_id = %context.revision_id,
            source_truth_version, ?error,
            "graph-refresh lifecycle committed but active graph summary refresh failed");
    }
}

fn log_graph_refresh_published(context: &IngestStageContext<'_>) {
    tracing::info!(worker_id = %context.worker_id, job_id = %context.job.id,
        attempt_id = %context.attempt_id, revision_id = %context.revision_id,
        "graph-refresh maintenance was atomically published");
}

fn log_graph_refresh_superseded(context: &IngestStageContext<'_>) {
    tracing::info!(worker_id = %context.worker_id, job_id = %context.job.id,
        attempt_id = %context.attempt_id, revision_id = %context.revision_id,
        "graph-refresh maintenance was superseded by a newer readable revision");
}

fn log_graph_refresh_authority_loss(context: &IngestStageContext<'_>) {
    warn!(worker_id = %context.worker_id, job_id = %context.job.id,
        attempt_id = %context.attempt_id, revision_id = %context.revision_id,
        "graph-refresh publication arrived after attempt authority moved");
}

fn log_graph_rebuild_failure(context: &IngestStageContext<'_>, graph_error: &impl std::fmt::Debug) {
    warn!(worker_id = %context.worker_id, job_id = %context.job.id,
        revision_id = %context.revision_id, ?graph_error, "graph rebuild failed");
}

fn log_graph_candidate_failure(context: &IngestStageContext<'_>, error: &impl std::fmt::Debug) {
    warn!(worker_id = %context.worker_id, job_id = %context.job.id,
        revision_id = %context.revision_id, ?error, "graph candidate extraction failed");
}

fn log_graph_extraction_degraded(context: &IngestStageContext<'_>, reason: &str) {
    warn!(worker_id = %context.worker_id, job_id = %context.job.id,
        revision_id = %context.revision_id, reason = %reason,
        "graph extraction degraded after provider retries; keeping embedded chunk vectors and promoting document as searchable (graph repair scheduled)");
}
