use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{content_document_repository, ingest_repository},
    interfaces::http::router_support::ApiError,
    services::{
        ingest::service::{
            FinalizeAttemptCommand, INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH,
            INGEST_STAGE_WEB_DISCOVERY, INGEST_STAGE_WEB_MATERIALIZE_PAGE,
            INGEST_STAGE_WEBHOOK_DELIVERY, LeaseAttemptCommand,
        },
        webhook::error::WebhookServiceError,
    },
};

use super::{
    errors::{DeletedDocumentJobSkipped, PipelineCancellation},
    graph::run_graph_refresh,
    heartbeat::{AttemptHeartbeatGuard, AttemptHeartbeatSignal, spawn_attempt_heartbeat_observer},
    lifecycle::{finalize_failed_ingest_job, finalize_successful_ingest_job},
    pipeline::run_ingest_pipeline,
    web_jobs::{run_web_discovery_job, run_web_materialize_page_job},
};

#[derive(Default)]
struct IngestDispatchOutcome {
    content_ingest_finalized: bool,
    graph_refresh_finalized: bool,
    webhook_ingest_finalized: bool,
}

impl IngestDispatchOutcome {
    const fn is_finalized(&self) -> bool {
        self.content_ingest_finalized
            || self.graph_refresh_finalized
            || self.webhook_ingest_finalized
    }
}

/// Marks a worker-leased attempt failed when the worker task exits without
/// reaching its own finalizer — a panic, or an abort while the runtime shuts
/// down. The worker's `AttemptHeartbeatGuard` only stops the heartbeat and
/// silently leaves the attempt `leased`; without this the attempt stays leased
/// until the stale-lease reaper requeues and redoes the whole ingest, and the
/// graph-extraction runtime rows it activated stay `accepted`/`running` and
/// `processing` forever.
///
/// The finalizer is async, so `drop` spawns a best-effort detached task over
/// the existing `IngestService::finalize_attempt` path. That path calls
/// `runtime_graph_repository::terminalize_runtime_rows_for_attempt` inside the
/// same transaction, so the attempt's runtime rows close with it. If no runtime
/// is available the attempt is left to the stale-lease reaper, which is the
/// documented last resort.
pub struct WorkerAttemptAbortGuard {
    state: Arc<AppState>,
    job_id: Uuid,
    attempt_id: Uuid,
    initial_stage: String,
    armed: bool,
}

impl WorkerAttemptAbortGuard {
    fn arm(state: Arc<AppState>, job_id: Uuid, attempt_id: Uuid, initial_stage: String) -> Self {
        Self { state, job_id, attempt_id, initial_stage, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Test seam: disarm exactly as the normal-finalize paths do, so a test can
    /// prove the explicit disarm suppresses the detached finalizer.
    #[cfg(feature = "test-support")]
    pub fn disarm_for_test(&mut self) {
        self.disarm();
    }
}

impl Drop for WorkerAttemptAbortGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        warn!(
            job_id = %self.job_id,
            attempt_id = %self.attempt_id,
            "ingest worker task exited before finalizing its leased attempt; marking it failed as worker_aborted",
        );
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                job_id = %self.job_id,
                attempt_id = %self.attempt_id,
                "no tokio runtime to finalize the aborted ingest attempt; the stale-lease reaper will recover it",
            );
            return;
        };
        let state = Arc::clone(&self.state);
        let attempt_id = self.attempt_id;
        let initial_stage = self.initial_stage.clone();
        runtime.spawn(async move {
            let finalize = state
                .application_services
                .ingest
                .finalize_attempt(
                    &state,
                    FinalizeAttemptCommand {
                        attempt_id,
                        knowledge_generation_id: None,
                        attempt_state: "failed".to_string(),
                        current_stage: Some(initial_stage),
                        failure_class: Some("worker_aborted".to_string()),
                        failure_code: Some("worker_aborted".to_string()),
                        failure_message: Some(
                            "ingest worker task exited abnormally".to_string(),
                        ),
                        retryable: true,
                        operational_defer: None,
                    },
                )
                .await;
            if let Err(error) = finalize {
                warn!(
                    attempt_id = %attempt_id,
                    ?error,
                    "best-effort worker_aborted finalization failed; the stale-lease reaper will recover the attempt",
                );
            }
        });
    }
}

/// Test seam: builds an armed guard around an already-leased attempt so a test
/// can prove the `Drop` finalizer closes the attempt and its runtime rows
/// without running the whole pipeline.
#[cfg(feature = "test-support")]
#[must_use]
pub fn worker_attempt_abort_guard_for_test(
    state: Arc<AppState>,
    job_id: Uuid,
    attempt_id: Uuid,
    initial_stage: String,
) -> WorkerAttemptAbortGuard {
    WorkerAttemptAbortGuard::arm(state, job_id, attempt_id, initial_stage)
}

/// Identity and cancellation handles shared by the stages of a leased attempt.
#[derive(Clone, Copy)]
pub(super) struct IngestStageContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) worker_id: &'a str,
    pub(super) job: &'a ingest_repository::IngestJobRow,
    pub(super) attempt_id: Uuid,
    pub(super) document_id: Uuid,
    pub(super) revision_id: Uuid,
    pub(super) cancellation: PipelineCancellation<'a>,
}

pub(super) struct IngestAttemptLifecycle<'a> {
    pub(super) state: &'a Arc<AppState>,
    pub(super) worker_id: &'a str,
    pub(super) job: &'a ingest_repository::IngestJobRow,
    pub(super) attempt_id: Uuid,
    pub(super) initial_stage: &'a str,
    pub(super) expected_queue_lease_token: &'a str,
    pub(super) cancellation: PipelineCancellation<'a>,
}

impl<'a> IngestAttemptLifecycle<'a> {
    /// The shared stage context for the pipeline stages this attempt dispatches.
    fn stage_context(&self, document_id: Uuid, revision_id: Uuid) -> IngestStageContext<'a> {
        IngestStageContext {
            state: self.state.as_ref(),
            worker_id: self.worker_id,
            job: self.job,
            attempt_id: self.attempt_id,
            document_id,
            revision_id,
            cancellation: self.cancellation,
        }
    }
}

async fn observe_initial_queue_state(lifecycle: &IngestAttemptLifecycle<'_>) -> anyhow::Result<()> {
    let current_job = ingest_repository::get_ingest_job_by_id(
        &lifecycle.state.persistence.postgres,
        lifecycle.job.id,
    )
    .await
    .context("failed to reload ingest job for cancellation check")?;
    let Some(current_job) = current_job else {
        return Ok(());
    };
    match current_job.queue_state.as_str() {
        "canceled" => lifecycle.cancellation.user_requested.store(true, Ordering::Relaxed),
        "paused" => lifecycle.cancellation.operator_pause_requested.store(true, Ordering::Relaxed),
        "leased" => return Ok(()),
        _ => lifecycle.cancellation.lease_lost_requested.store(true, Ordering::Relaxed),
    }
    lifecycle.cancellation.token.cancel();
    Ok(())
}

async fn dispatch_content_mutation(lifecycle: &IngestAttemptLifecycle<'_>) -> anyhow::Result<()> {
    let revision_id = lifecycle
        .job
        .knowledge_revision_id
        .context("ingest job is missing knowledge_revision_id")?;
    let document_id = lifecycle
        .job
        .knowledge_document_id
        .context("ingest job is missing knowledge_document_id")?;
    let document = content_document_repository::get_document_by_id(
        &lifecycle.state.persistence.postgres,
        document_id,
    )
    .await
    .map_err(|_| anyhow::anyhow!("failed to load document"))?;
    if !document.as_ref().is_some_and(|document| document.document_state == "deleted") {
        Box::pin(run_ingest_pipeline(lifecycle.stage_context(document_id, revision_id))).await?;
        return Ok(());
    }
    if let Some(mutation_id) = lifecycle.job.mutation_id {
        lifecycle
            .state
            .application_services
            .content
            .settle_deleted_document_mutation(lifecycle.state, mutation_id)
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to settle skipped mutation for deleted document: {error}")
            })?;
    }
    info!(document_id = %document_id, "canceling leased ingest for deleted document");
    Err(anyhow::Error::new(DeletedDocumentJobSkipped { document_id }))
}

async fn dispatch_graph_refresh(lifecycle: &IngestAttemptLifecycle<'_>) -> anyhow::Result<()> {
    let revision_id = lifecycle
        .job
        .knowledge_revision_id
        .context("graph-refresh job is missing knowledge_revision_id")?;
    let document_id = lifecycle
        .job
        .knowledge_document_id
        .context("graph-refresh job is missing knowledge_document_id")?;
    run_graph_refresh(lifecycle.stage_context(document_id, revision_id)).await?;
    Ok(())
}

async fn dispatch_webhook_delivery(lifecycle: &IngestAttemptLifecycle<'_>) -> anyhow::Result<bool> {
    crate::services::webhook::delivery::run_webhook_delivery_job(
        lifecycle.state,
        lifecycle.job,
        lifecycle.attempt_id,
        lifecycle.expected_queue_lease_token,
        lifecycle.cancellation.token,
    )
    .await
    .map(|outcome| {
        matches!(
            outcome,
            crate::services::webhook::delivery::WebhookDeliveryJobOutcome::IngestAlreadyFinalized
        )
    })
    .map_err(|error| {
        if matches!(&error, WebhookServiceError::DeliveryCanceled { .. }) {
            lifecycle.cancellation.error(lifecycle.job.id)
        } else {
            error.into()
        }
    })
}

async fn dispatch_ingest_job(
    lifecycle: &IngestAttemptLifecycle<'_>,
) -> anyhow::Result<IngestDispatchOutcome> {
    observe_initial_queue_state(lifecycle).await?;
    lifecycle.cancellation.check(lifecycle.job.id)?;
    let mut outcome = IngestDispatchOutcome::default();
    match lifecycle.job.job_kind.as_str() {
        "content_mutation" => {
            Box::pin(dispatch_content_mutation(lifecycle)).await?;
            outcome.content_ingest_finalized = true;
        }
        "graph_refresh" => {
            dispatch_graph_refresh(lifecycle).await?;
            outcome.graph_refresh_finalized = true;
        }
        "web_discovery" => {
            Box::pin(run_web_discovery_job(lifecycle.state, lifecycle.job, lifecycle.attempt_id))
                .await?;
        }
        "web_materialize_page" => {
            run_web_materialize_page_job(lifecycle.state, lifecycle.job, lifecycle.attempt_id)
                .await?;
        }
        "webhook_delivery" => {
            outcome.webhook_ingest_finalized = dispatch_webhook_delivery(lifecycle).await?;
        }
        other => anyhow::bail!("unsupported ingest job kind {other}"),
    }
    Ok(outcome)
}

fn initial_ingest_stage(job_kind: &str) -> anyhow::Result<&'static str> {
    match job_kind {
        "content_mutation" => Ok(INGEST_STAGE_EXTRACT_CONTENT),
        "graph_refresh" => Ok(INGEST_STAGE_EXTRACT_GRAPH),
        "web_discovery" => Ok(INGEST_STAGE_WEB_DISCOVERY),
        "web_materialize_page" => Ok(INGEST_STAGE_WEB_MATERIALIZE_PAGE),
        "webhook_delivery" => Ok(INGEST_STAGE_WEBHOOK_DELIVERY),
        other => anyhow::bail!("unsupported ingest job kind {other}"),
    }
}

async fn lease_ingest_attempt(
    state: &Arc<AppState>,
    worker_id: &str,
    job_id: Uuid,
    initial_stage: &str,
    expected_queue_lease_token: &str,
) -> anyhow::Result<Option<Uuid>> {
    match state
        .application_services
        .ingest
        .lease_attempt(
            state,
            LeaseAttemptCommand {
                job_id,
                worker_principal_id: None,
                lease_token: Some(format!("worker-{worker_id}-{}", Uuid::now_v7())),
                expected_queue_lease_token: Some(expected_queue_lease_token.to_string()),
                knowledge_generation_id: None,
                current_stage: Some(initial_stage.to_string()),
            },
        )
        .await
    {
        Ok(attempt) => Ok(Some(attempt.id)),
        Err(ApiError::Conflict(message)) => {
            warn!(%worker_id, %job_id, %message, "queue lease moved before ingest attempt creation");
            Ok(None)
        }
        Err(error) => Err(error).context("failed to lease ingest attempt"),
    }
}

pub(super) async fn execute_ingest_job(
    state: Arc<AppState>,
    worker_id: &str,
    job: ingest_repository::IngestJobRow,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let job_id = job.id;
    let expected_queue_lease_token =
        job.queue_lease_token.clone().context("claimed ingest job is missing queue lease token")?;
    let initial_stage = initial_ingest_stage(&job.job_kind)?.to_string();
    let Some(attempt_id) = lease_ingest_attempt(
        &state,
        worker_id,
        job_id,
        &initial_stage,
        &expected_queue_lease_token,
    )
    .await?
    else {
        return Ok(());
    };
    let mut abort_guard =
        WorkerAttemptAbortGuard::arm(Arc::clone(&state), job.id, attempt_id, initial_stage.clone());

    let heartbeat_running = Arc::new(AtomicBool::new(true));
    let heartbeat_guard = AttemptHeartbeatGuard::new(Arc::clone(&heartbeat_running));
    let user_cancel_requested = Arc::new(AtomicBool::new(false));
    let operator_pause_requested = Arc::new(AtomicBool::new(false));
    let lease_lost_requested = Arc::new(AtomicBool::new(false));

    let heartbeat_interval =
        Duration::from_secs(state.settings.ingestion_worker_heartbeat_interval_seconds.max(1));
    spawn_attempt_heartbeat_observer(
        state.persistence.heartbeat_postgres.clone(),
        attempt_id,
        job.id,
        heartbeat_interval,
        Arc::clone(&heartbeat_running),
        AttemptHeartbeatSignal {
            cancellation_token: cancellation_token.clone(),
            user_cancel_requested: Arc::clone(&user_cancel_requested),
            operator_pause_requested: Arc::clone(&operator_pause_requested),
            lease_lost_requested: Arc::clone(&lease_lost_requested),
        },
    );

    let cancellation = PipelineCancellation {
        token: &cancellation_token,
        user_requested: &user_cancel_requested,
        operator_pause_requested: &operator_pause_requested,
        lease_lost_requested: &lease_lost_requested,
    };
    let lifecycle = IngestAttemptLifecycle {
        state: &state,
        worker_id,
        job: &job,
        attempt_id,
        initial_stage: &initial_stage,
        expected_queue_lease_token: &expected_queue_lease_token,
        cancellation,
    };
    let dispatch_result = Box::pin(dispatch_ingest_job(&lifecycle)).await;
    let already_finalized = dispatch_result.as_ref().is_ok_and(IngestDispatchOutcome::is_finalized);
    let result = dispatch_result.map(|_| ());
    drop(heartbeat_guard);

    if already_finalized && result.is_ok() {
        abort_guard.disarm();
        info!(
            %worker_id,
            %job_id,
            %attempt_id,
            "ingest job atomically finalized its current lease"
        );
        return Ok(());
    }

    abort_guard.disarm();
    match result {
        Ok(()) => finalize_successful_ingest_job(&lifecycle).await,
        Err(error) => finalize_failed_ingest_job(&lifecycle, error).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_stage_matches_each_dispatch_kind_and_rejects_unknown_jobs() {
        for (kind, stage) in [
            ("content_mutation", INGEST_STAGE_EXTRACT_CONTENT),
            ("graph_refresh", INGEST_STAGE_EXTRACT_GRAPH),
            ("web_discovery", INGEST_STAGE_WEB_DISCOVERY),
            ("web_materialize_page", INGEST_STAGE_WEB_MATERIALIZE_PAGE),
            ("webhook_delivery", INGEST_STAGE_WEBHOOK_DELIVERY),
        ] {
            assert_eq!(initial_ingest_stage(kind).unwrap(), stage);
        }
        assert_eq!(
            initial_ingest_stage("unsupported").unwrap_err().to_string(),
            "unsupported ingest job kind unsupported",
        );
    }
}
