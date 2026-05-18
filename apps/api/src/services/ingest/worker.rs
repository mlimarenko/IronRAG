mod extraction;
mod failure;
mod runtime;
mod web_jobs;

use std::{
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::Utc;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, broadcast},
    task::JoinHandle,
    time,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{content_repository, ingest_repository},
    integrations::docling,
    services::{
        content::service::{
            MaterializeRevisionGraphCandidatesCommand, PromoteHeadCommand,
            fail_revision_vector_graph_readiness, graph_extract_success_message,
            graph_state_after_successful_extract,
        },
        ingest::cancellation::anyhow_is_cancelled,
        ingest::service::{
            FinalizeAttemptCommand, INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EMBED_CHUNK,
            INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH,
            INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_FINALIZING,
            INGEST_STAGE_PREPARE_STRUCTURE, INGEST_STAGE_WEB_DISCOVERY,
            INGEST_STAGE_WEB_MATERIALIZE_PAGE, INGEST_STAGE_WEBHOOK_DELIVERY, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
    },
    shared::{
        extraction::file_extract::{FileExtractionPlan, UploadAdmissionError},
        telemetry,
    },
};

use self::{
    extraction::{
        generate_document_summary_from_blocks, resolve_canonical_extract_content,
        sync_resumable_pdf_extract_stage_progress_from_units,
    },
    failure::fail_canonical_ingest_job,
    runtime::run_ingestion_worker_pool,
    web_jobs::{run_canonical_web_discovery_job, run_canonical_web_materialize_page_job},
};

/// How often each worker polls the ingest queue for new jobs.
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the lease-recovery sweep runs to reclaim stale leases.
const CANONICAL_LEASE_RECOVERY_INTERVAL: Duration = Duration::from_secs(15);
// Steady-state stale-lease threshold. Was 120s; that is 8× the heartbeat
// interval and lets the dispatcher self-deadlock for two minutes after a
// worker crashes. 60s = 4× heartbeat, still safe against transient DB
// latency, and gets the queue moving again much faster.
const CANONICAL_STALE_LEASE_SECONDS: i64 = 60;
/// Aggressive threshold used **only** for the one-shot sweep that runs when
/// the worker pool boots. At pool startup we know nothing in this process is
/// currently holding a lease, so any `leased` row older than two heartbeat
/// intervals is guaranteed to be orphaned by a previous process that crashed
/// or was restarted before it could finalize. We pick a threshold well above
/// the heartbeat interval (`CANONICAL_HEARTBEAT_INTERVAL`) so a healthy
/// sibling worker in a multi-worker deployment is never falsely reclaimed.
const CANONICAL_STARTUP_LEASE_RECOVERY_SECONDS: i64 = 30;
const DEFAULT_HEAVY_REVISION_BYTES: i64 = 8 * 1024 * 1024;
const HEAVY_PIPELINE_AUTO_MAX_PARALLELISM: usize = 4;
const HEAVY_PIPELINE_AUTO_RESERVED_MEMORY_MIB: u64 = 2048;
const HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB: u64 = 1024;
const HEAVY_PIPELINE_AUTO_DOCLING_WAITERS_PER_PROCESS: usize = 2;

static HEAVY_REVISION_PIPELINE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(heavy_revision_pipeline_parallelism())));

struct AttemptHeartbeatGuard {
    running: Arc<AtomicBool>,
}

impl AttemptHeartbeatGuard {
    fn new(running: Arc<AtomicBool>) -> Self {
        Self { running }
    }
}

impl Drop for AttemptHeartbeatGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub(super) struct CanonicalExtractContentError {
    failure_code: String,
    retryable: bool,
    message: String,
}

#[derive(Debug, Error)]
#[error("document {document_id} was deleted before ingest could run")]
struct DeletedDocumentJobSkipped {
    document_id: Uuid,
}

/// Raised from inside the pipeline when the heartbeat observer notices that
/// the job has been transitioned to `queue_state='canceled'` by the cancel
/// endpoint (`cancel_jobs_for_document`). The cancel path has already marked
/// the job and leased attempt as canceled, so the worker only stops cleanly
/// and must not rewrite the terminal cancellation state.
#[derive(Debug, Error)]
#[error("canonical ingest job {job_id} was canceled by user request")]
struct JobCanceledByRequest {
    job_id: Uuid,
}

#[derive(Debug, Error)]
#[error("canonical ingest job {job_id} was paused by operator request")]
struct JobPausedByOperator {
    job_id: Uuid,
}

#[derive(Debug, Error)]
#[error("canonical ingest job {job_id} stopped because worker shutdown was requested")]
struct JobCanceledByShutdown {
    job_id: Uuid,
}

#[derive(Debug, Error)]
#[error("canonical ingest job {job_id} lost its active attempt lease")]
struct JobLeaseLost {
    job_id: Uuid,
}

fn job_cancellation_error(
    job_id: Uuid,
    user_cancel_requested: &AtomicBool,
    operator_pause_requested: &AtomicBool,
    lease_lost_requested: &AtomicBool,
) -> anyhow::Error {
    if user_cancel_requested.load(Ordering::Relaxed) {
        anyhow::Error::new(JobCanceledByRequest { job_id })
    } else if operator_pause_requested.load(Ordering::Relaxed) {
        anyhow::Error::new(JobPausedByOperator { job_id })
    } else if lease_lost_requested.load(Ordering::Relaxed) {
        anyhow::Error::new(JobLeaseLost { job_id })
    } else {
        anyhow::Error::new(JobCanceledByShutdown { job_id })
    }
}

fn check_job_cancellation(
    cancellation_token: &CancellationToken,
    user_cancel_requested: &AtomicBool,
    operator_pause_requested: &AtomicBool,
    lease_lost_requested: &AtomicBool,
    job_id: Uuid,
) -> anyhow::Result<()> {
    if cancellation_token.is_cancelled() {
        Err(job_cancellation_error(
            job_id,
            user_cancel_requested,
            operator_pause_requested,
            lease_lost_requested,
        ))
    } else {
        Ok(())
    }
}

async fn acquire_heavy_revision_pipeline_permit(
    revision: &content_repository::ContentRevisionRow,
) -> anyhow::Result<Option<OwnedSemaphorePermit>> {
    if !is_heavy_revision_pipeline_job(revision) {
        return Ok(None);
    }
    let permit = HEAVY_REVISION_PIPELINE
        .clone()
        .acquire_owned()
        .await
        .context("heavy revision pipeline limiter is closed")?;
    Ok(Some(permit))
}

fn is_heavy_revision_pipeline_job(revision: &content_repository::ContentRevisionRow) -> bool {
    let mime_type = revision
        .mime_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    mime_type == "application/pdf" && revision.byte_size >= heavy_revision_byte_threshold()
}

fn heavy_revision_byte_threshold() -> i64 {
    std::env::var("IRONRAG_INGESTION_HEAVY_REVISION_BYTES")
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_HEAVY_REVISION_BYTES)
}

fn heavy_revision_pipeline_parallelism() -> usize {
    let raw = std::env::var("IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM").ok();
    match raw.as_deref().map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("auto") || value.is_empty() => {
            auto_heavy_revision_pipeline_parallelism()
        }
        Some(value) => match value.parse::<usize>().ok().filter(|value| *value > 0) {
            Some(value) => {
                tracing::info!(
                    parallelism = value,
                    "heavy revision pipeline parallelism configured"
                );
                value
            }
            None => {
                tracing::warn!(
                    raw = value,
                    fallback_parallelism = 1,
                    "invalid IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM; using fail-safe heavy revision pipeline parallelism"
                );
                1
            }
        },
        None => auto_heavy_revision_pipeline_parallelism(),
    }
}

fn auto_heavy_revision_pipeline_parallelism() -> usize {
    let cpu_parallelism = telemetry::detect_container_cpu_parallelism().unwrap_or(1);
    let memory_limit_bytes = telemetry::detect_container_memory_limit_bytes();
    let docling_parallelism = docling::configured_max_concurrency();
    let parallelism = auto_heavy_revision_pipeline_parallelism_for_limits(
        cpu_parallelism,
        memory_limit_bytes,
        docling_parallelism,
    );
    let memory_limit_mib = memory_limit_bytes.map(|bytes| bytes / (1024 * 1024));
    let soft_limit_mib = memory_limit_mib.map(|mib| mib.saturating_mul(9) / 10);
    let heavy_budget_mib =
        soft_limit_mib.map(|mib| mib.saturating_sub(HEAVY_PIPELINE_AUTO_RESERVED_MEMORY_MIB));
    tracing::info!(
        cpu_parallelism,
        ?memory_limit_mib,
        ?soft_limit_mib,
        ?heavy_budget_mib,
        reserved_mib = HEAVY_PIPELINE_AUTO_RESERVED_MEMORY_MIB,
        per_job_mib = HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB,
        max_parallelism = HEAVY_PIPELINE_AUTO_MAX_PARALLELISM,
        docling_parallelism,
        docling_waiters_per_process = HEAVY_PIPELINE_AUTO_DOCLING_WAITERS_PER_PROCESS,
        parallelism,
        "heavy revision pipeline auto parallelism resolved"
    );
    if heavy_budget_mib.is_some_and(|budget| budget < HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB) {
        tracing::warn!(
            ?memory_limit_mib,
            ?soft_limit_mib,
            ?heavy_budget_mib,
            required_mib = HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB,
            "heavy revision pipeline auto parallelism has only enough memory budget for the mandatory single job"
        );
    }
    parallelism
}

fn auto_heavy_revision_pipeline_parallelism_for_limits(
    cpu_parallelism: usize,
    memory_limit_bytes: Option<u64>,
    docling_parallelism: usize,
) -> usize {
    let cpu_bound = cpu_parallelism.clamp(1, HEAVY_PIPELINE_AUTO_MAX_PARALLELISM);
    let docling_bound = docling_parallelism
        .max(1)
        .saturating_mul(HEAVY_PIPELINE_AUTO_DOCLING_WAITERS_PER_PROCESS)
        .min(HEAVY_PIPELINE_AUTO_MAX_PARALLELISM);
    let memory_bound = memory_limit_bytes
        .map(|bytes| bytes / (1024 * 1024))
        .map(|memory_mib| {
            let soft_limit_mib = memory_mib.saturating_mul(9) / 10;
            soft_limit_mib
                .saturating_sub(HEAVY_PIPELINE_AUTO_RESERVED_MEMORY_MIB)
                .checked_div(HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB)
                .unwrap_or(0) as usize
        })
        .unwrap_or(1);

    cpu_bound
        .min(docling_bound)
        .min(memory_bound.max(1))
        .clamp(1, HEAVY_PIPELINE_AUTO_MAX_PARALLELISM)
}

fn map_stage_error(
    error: anyhow::Error,
    user_cancel_requested: &AtomicBool,
    operator_pause_requested: &AtomicBool,
    lease_lost_requested: &AtomicBool,
    job_id: Uuid,
    context: &'static str,
) -> anyhow::Error {
    if anyhow_is_cancelled(&error) {
        job_cancellation_error(
            job_id,
            user_cancel_requested,
            operator_pause_requested,
            lease_lost_requested,
        )
    } else {
        error.context(context)
    }
}

impl CanonicalExtractContentError {
    fn missing_stored_source(job_id: Uuid, revision_id: Uuid) -> Self {
        Self {
            failure_code: "missing_stored_source".to_string(),
            retryable: false,
            message: format!(
                "canonical ingest job {job_id}: revision {revision_id} has no normalized_text and no stored source bytes",
            ),
        }
    }

    fn stored_source_read(storage_ref: &str, error: impl std::fmt::Display) -> Self {
        Self {
            failure_code: "stored_source_unavailable".to_string(),
            retryable: false,
            message: format!("failed to read stored source {storage_ref}: {error}"),
        }
    }

    fn extraction_rejected(rejection: &UploadAdmissionError) -> Self {
        Self {
            failure_code: rejection.error_kind().to_string(),
            retryable: false,
            message: rejection.message().to_string(),
        }
    }

    fn extraction_failed(failure_code: &str, message: impl std::fmt::Display) -> Self {
        Self {
            failure_code: failure_code.to_string(),
            retryable: true,
            message: message.to_string(),
        }
    }
}

impl std::fmt::Display for CanonicalExtractContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CanonicalExtractContentError {}

pub(super) struct CanonicalExtractedContent {
    extraction_plan: FileExtractionPlan,
    stage_details: serde_json::Value,
    provider_kind: Option<String>,
    model_name: Option<String>,
    usage_json: serde_json::Value,
}

pub fn spawn_ingestion_worker(
    state: AppState,
    shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_ingestion_worker_pool(Arc::new(state), shutdown).await;
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_attempt_heartbeat_observer(
    heartbeat_pg: sqlx::PgPool,
    attempt_id: Uuid,
    job_id: Uuid,
    heartbeat_interval: Duration,
    heartbeat_running: Arc<AtomicBool>,
    cancellation_token: CancellationToken,
    user_cancel_requested: Arc<AtomicBool>,
    operator_pause_requested: Arc<AtomicBool>,
    lease_lost_requested: Arc<AtomicBool>,
) {
    let thread_name = format!("ironrag-heartbeat-{}", attempt_id.simple());
    let spawn_failure_cancellation = cancellation_token.clone();
    let spawn_failure_lease_lost = Arc::clone(&lease_lost_requested);
    let spawn_result = std::thread::Builder::new().name(thread_name).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(%error, %attempt_id, "attempt heartbeat observer failed to start runtime");
                lease_lost_requested.store(true, Ordering::Relaxed);
                cancellation_token.cancel();
                return;
            }
        };

        runtime.block_on(async move {
            // Runs outside the main Tokio runtime so CPU-heavy graph reconcile
            // cannot starve heartbeats and trigger a false stale-lease requeue.
            while heartbeat_running.load(Ordering::Relaxed) {
                tokio::time::sleep(heartbeat_interval).await;
                if !heartbeat_running.load(Ordering::Relaxed) {
                    break;
                }

                match ingest_repository::touch_attempt_heartbeat_and_load_job_state(
                    &heartbeat_pg,
                    attempt_id,
                    None,
                )
                .await
                {
                    Ok(Some(queue_state)) if queue_state == "leased" => {}
                    Ok(Some(queue_state)) if queue_state == "canceled" => {
                        info!(
                            %job_id,
                            %attempt_id,
                            "cancellation observed on heartbeat tick, signalling pipeline abort"
                        );
                        user_cancel_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                    }
                    Ok(Some(queue_state)) if queue_state == "paused" => {
                        info!(
                            %job_id,
                            %attempt_id,
                            "operator pause observed on heartbeat tick, signalling pipeline pause"
                        );
                        operator_pause_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                    }
                    Ok(Some(queue_state)) => {
                        lease_lost_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                        warn!(
                            %job_id,
                            %attempt_id,
                            queue_state = %queue_state,
                            "attempt heartbeat observed job lease moved away; cancelling stale worker pipeline"
                        );
                        break;
                    }
                    Ok(None) => {
                        lease_lost_requested.store(true, Ordering::Relaxed);
                        cancellation_token.cancel();
                        warn!(
                            %job_id,
                            %attempt_id,
                            "attempt heartbeat observed lost lease; cancelling stale worker pipeline"
                        );
                        break;
                    }
                    Err(error) => {
                        warn!(
                            ?error,
                            %attempt_id,
                            "failed to touch attempt heartbeat and poll queue state"
                        );
                    }
                }
            }
        });
    });
    if let Err(error) = spawn_result {
        tracing::error!(%error, %attempt_id, "failed to spawn attempt heartbeat observer");
        spawn_failure_lease_lost.store(true, Ordering::Relaxed);
        spawn_failure_cancellation.cancel();
    }
}

async fn execute_canonical_ingest_job(
    state: Arc<AppState>,
    worker_id: &str,
    job: ingest_repository::IngestJobRow,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let job_id = job.id;
    let initial_stage = match job.job_kind.as_str() {
        "content_mutation" => INGEST_STAGE_EXTRACT_CONTENT.to_string(),
        "web_discovery" => INGEST_STAGE_WEB_DISCOVERY.to_string(),
        "web_materialize_page" => INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
        "webhook_delivery" => INGEST_STAGE_WEBHOOK_DELIVERY.to_string(),
        other => anyhow::bail!("unsupported canonical ingest job kind {other}"),
    };

    let attempt = state
        .canonical_services
        .ingest
        .lease_attempt(
            &state,
            LeaseAttemptCommand {
                job_id,
                worker_principal_id: None,
                lease_token: Some(format!("worker-{worker_id}-{}", Uuid::now_v7())),
                knowledge_generation_id: None,
                current_stage: Some(initial_stage.clone()),
            },
        )
        .await
        .context("failed to lease canonical ingest attempt")?;

    let attempt_id = attempt.id;

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
        cancellation_token.clone(),
        Arc::clone(&user_cancel_requested),
        Arc::clone(&operator_pause_requested),
        Arc::clone(&lease_lost_requested),
    );

    // Pre-lease cancellation guard: a job may have been canceled *between*
    // `claim_next_queued_ingest_job` and the point where the heartbeat loop
    // starts observing. Fold the first observation into the same path we use
    // mid-pipeline so there is exactly one cancel handling branch.
    let current_job = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job.id)
        .await
        .context("failed to reload ingest job for cancellation check")?;
    if current_job.as_ref().is_some_and(|j| j.queue_state == "canceled") {
        user_cancel_requested.store(true, Ordering::Relaxed);
        cancellation_token.cancel();
    } else if current_job.as_ref().is_some_and(|j| j.queue_state == "paused") {
        operator_pause_requested.store(true, Ordering::Relaxed);
        cancellation_token.cancel();
    } else if current_job.as_ref().is_some_and(|j| j.queue_state != "leased") {
        lease_lost_requested.store(true, Ordering::Relaxed);
        cancellation_token.cancel();
    }

    let result = if cancellation_token.is_cancelled() {
        Err(job_cancellation_error(
            job.id,
            &user_cancel_requested,
            &operator_pause_requested,
            &lease_lost_requested,
        ))
    } else {
        match job.job_kind.as_str() {
            "content_mutation" => {
                let revision_id = job
                    .knowledge_revision_id
                    .context("canonical ingest job is missing knowledge_revision_id")?;
                let document_id = job
                    .knowledge_document_id
                    .context("canonical ingest job is missing knowledge_document_id")?;

                // Check if document was deleted while job was queued
                let document = content_repository::get_document_by_id(
                    &state.persistence.postgres,
                    document_id,
                )
                .await
                .map_err(|_| anyhow::anyhow!("failed to load document"))?;
                if document.as_ref().is_some_and(|d| d.document_state == "deleted") {
                    if let Some(mutation_id) = job.mutation_id {
                        state
                            .canonical_services
                            .content
                            .settle_deleted_document_mutation(&state, mutation_id)
                            .await
                            .map_err(|error| {
                                anyhow::anyhow!(
                                    "failed to settle skipped mutation for deleted document: {error}"
                                )
                            })?;
                    }
                    info!(document_id = %document_id, "canceling leased ingest for deleted document");
                    return Err(anyhow::Error::new(DeletedDocumentJobSkipped { document_id }));
                }

                run_canonical_ingest_pipeline(
                    &state,
                    worker_id,
                    &job,
                    attempt_id,
                    document_id,
                    revision_id,
                    &cancellation_token,
                    &user_cancel_requested,
                    &operator_pause_requested,
                    &lease_lost_requested,
                )
                .await
            }
            "web_discovery" => run_canonical_web_discovery_job(&state, &job, attempt_id).await,
            "web_materialize_page" => {
                run_canonical_web_materialize_page_job(&state, &job, attempt_id).await
            }
            "webhook_delivery" => {
                crate::services::webhook::delivery::run_webhook_delivery_job(&state, &job)
                    .await
                    .map_err(Into::into)
            }
            other => Err(anyhow::anyhow!("unsupported canonical ingest job kind {other}")),
        }
    };

    drop(heartbeat_guard);

    match result {
        Ok(()) => {
            let finalize_result = state
                .canonical_services
                .ingest
                .finalize_attempt(
                    &state,
                    FinalizeAttemptCommand {
                        attempt_id,
                        knowledge_generation_id: None,
                        attempt_state: "succeeded".to_string(),
                        current_stage: Some(match job.job_kind.as_str() {
                            "content_mutation" => INGEST_STAGE_FINALIZING.to_string(),
                            "web_discovery" => INGEST_STAGE_WEB_DISCOVERY.to_string(),
                            "web_materialize_page" => INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
                            "webhook_delivery" => INGEST_STAGE_WEBHOOK_DELIVERY.to_string(),
                            _ => initial_stage.clone(),
                        }),
                        failure_class: None,
                        failure_code: None,
                        failure_message: None,
                        retryable: false,
                    },
                )
                .await;
            if let Err(error) = finalize_result {
                if matches!(error, crate::interfaces::http::router_support::ApiError::Conflict(_)) {
                    match ingest_repository::get_ingest_job_by_id(
                        &state.persistence.postgres,
                        job_id,
                    )
                    .await
                    {
                        Ok(Some(row)) if row.queue_state == "paused" => {
                            if let Err(e) = ingest_repository::abandon_paused_ingest_attempt(
                                &state.persistence.postgres,
                                attempt_id,
                            )
                            .await
                            {
                                tracing::warn!(%attempt_id, ?e, "failed to finalize paused ingest attempt after successful pipeline return");
                            }
                            info!(
                                %worker_id,
                                %job_id,
                                %attempt_id,
                                "canonical ingest job completed after operator pause; preserving paused queue state"
                            );
                            return Ok(());
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(%attempt_id, ?e, "failed to reload ingest job after finalize conflict");
                        }
                    }
                    warn!(
                        %worker_id,
                        %job_id,
                        %attempt_id,
                        ?error,
                        "canonical ingest job finished after losing its active lease; leaving queue state to the current owner"
                    );
                    return Ok(());
                }
                return Err(error)
                    .context("failed to finalize canonical ingest attempt as succeeded");
            }
            info!(
                %worker_id,
                %job_id,
                %attempt_id,
                "canonical ingest job completed",
            );
            Ok(())
        }
        Err(error) => {
            if error.downcast_ref::<JobCanceledByRequest>().is_some() {
                info!(
                    %worker_id,
                    %job_id,
                    %attempt_id,
                    "canonical ingest job observed user cancel request and stopped cooperatively",
                );
                return Ok(());
            }
            if error.downcast_ref::<JobPausedByOperator>().is_some() {
                if let Err(e) = ingest_repository::abandon_paused_ingest_attempt(
                    &state.persistence.postgres,
                    attempt_id,
                )
                .await
                {
                    tracing::warn!(%attempt_id, ?e, "failed to finalize paused ingest attempt");
                }
                info!(
                    %worker_id,
                    %job_id,
                    %attempt_id,
                    "canonical ingest job observed operator pause request and stopped cooperatively",
                );
                return Ok(());
            }
            if error.downcast_ref::<JobLeaseLost>().is_some() {
                warn!(
                    %worker_id,
                    %job_id,
                    %attempt_id,
                    "canonical ingest job stopped because its attempt lease was lost"
                );
                return Ok(());
            }
            if error.downcast_ref::<JobCanceledByShutdown>().is_some() {
                match ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id)
                    .await
                {
                    Ok(Some(row)) if row.queue_state == "canceled" => {
                        info!(
                            %worker_id,
                            %job_id,
                            %attempt_id,
                            "canonical ingest job stopped during shutdown after user cancel won the race",
                        );
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            %attempt_id,
                            ?e,
                            "failed to reload ingest job while finalizing shutdown cancellation"
                        );
                    }
                }
                if let Err(e) = state
                    .canonical_services
                    .ingest
                    .finalize_attempt(
                        &state,
                        FinalizeAttemptCommand {
                            attempt_id,
                            knowledge_generation_id: None,
                            attempt_state: "failed".to_string(),
                            current_stage: Some(initial_stage.clone()),
                            failure_class: Some("worker_shutdown".to_string()),
                            failure_code: Some("shutdown_cancelled".to_string()),
                            failure_message: Some(
                                "Worker shutdown canceled document processing".to_string(),
                            ),
                            retryable: true,
                        },
                    )
                    .await
                {
                    tracing::warn!(%attempt_id, ?e, "failed to requeue shutdown-canceled attempt");
                }
                info!(
                    %worker_id,
                    %job_id,
                    %attempt_id,
                    "canonical ingest job stopped cooperatively for worker shutdown",
                );
                return Ok(());
            }
            if error.downcast_ref::<DeletedDocumentJobSkipped>().is_some() {
                if let Err(e) = state
                    .canonical_services
                    .ingest
                    .finalize_attempt(
                        &state,
                        FinalizeAttemptCommand {
                            attempt_id,
                            knowledge_generation_id: None,
                            attempt_state: "canceled".to_string(),
                            current_stage: Some(initial_stage.clone()),
                            failure_class: Some("content_mutation".to_string()),
                            failure_code: Some("document_deleted".to_string()),
                            failure_message: Some(
                                "Document was deleted before processing finished".to_string(),
                            ),
                            retryable: false,
                        },
                    )
                    .await
                {
                    tracing::warn!(%attempt_id, ?e, "failed to finalize deleted-document attempt as canceled");
                }
                info!(%worker_id, %job_id, %attempt_id, "canonical ingest job canceled because document was deleted");
                return Ok(());
            }
            let message = format!("{error:#}");
            let extract_error = error.downcast_ref::<CanonicalExtractContentError>();
            if let Err(e) = state
                .canonical_services
                .ingest
                .finalize_attempt(
                    &state,
                    FinalizeAttemptCommand {
                        attempt_id,
                        knowledge_generation_id: None,
                        attempt_state: "failed".to_string(),
                        current_stage: Some(initial_stage.clone()),
                        failure_class: Some(
                            match job.job_kind.as_str() {
                                "content_mutation" if extract_error.is_some() => "content_extract",
                                "web_discovery" => "web_discovery",
                                "web_materialize_page" => "web_page_materialization",
                                _ => "worker_error",
                            }
                            .to_string(),
                        ),
                        failure_code: Some(
                            extract_error
                                .map(|failure| failure.failure_code.clone())
                                .unwrap_or_else(|| match job.job_kind.as_str() {
                                    "web_discovery" => "web_discovery_failed".to_string(),
                                    "web_materialize_page" => {
                                        "web_materialize_page_failed".to_string()
                                    }
                                    _ => "canonical_pipeline_failed".to_string(),
                                }),
                        ),
                        failure_message: Some(message.clone()),
                        retryable: extract_error.map(|failure| failure.retryable).unwrap_or(true),
                    },
                )
                .await
            {
                tracing::warn!(%attempt_id, ?e, "failed to finalize attempt as failed");
            }
            Err(error).context(message)
        }
    }
}

async fn run_canonical_ingest_pipeline(
    state: &AppState,
    worker_id: &str,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    cancellation_token: &CancellationToken,
    user_cancel_requested: &AtomicBool,
    operator_pause_requested: &AtomicBool,
    lease_lost_requested: &AtomicBool,
) -> anyhow::Result<()> {
    // --- Stage: extract_content -----------------------------------------------
    check_job_cancellation(
        cancellation_token,
        user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
        job.id,
    )?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                stage_state: "started".to_string(),
                message: None,
                details_json: serde_json::json!({}),
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
        .context("failed to record extract_content started stage event")?;

    let extract_content_start = Instant::now();

    // Read revision metadata from Postgres — the canonical source of truth.
    // ArangoDB is a derived cache; if the revision hasn't been mirrored yet
    // (e.g. after an ArangoDB outage during upload) populate it here so the
    // pipeline has a consistent view.
    let revision_row =
        content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
            .await
            .context("failed to load knowledge revision from postgres")?
            .with_context(|| format!("knowledge revision {revision_id} not found in postgres"))?;
    if let Err(error) =
        sync_resumable_pdf_extract_stage_progress_from_units(state, attempt_id, revision_id).await
    {
        warn!(
            %attempt_id,
            %revision_id,
            ?error,
            "failed to sync resumable extract_content progress before heavy revision wait"
        );
    }
    let heavy_revision_pipeline_permit =
        acquire_heavy_revision_pipeline_permit(&revision_row).await?;
    let heavy_revision_pipeline_limited = heavy_revision_pipeline_permit.is_some();
    if heavy_revision_pipeline_limited {
        tracing::info!(
            %revision_id,
            byte_size = revision_row.byte_size,
            "heavy revision pipeline slot acquired"
        );
    }

    let arango_revision = state
        .arango_document_store
        .get_revision(revision_id)
        .await
        .context("failed to load knowledge revision from arango")?;

    let revision = if let Some(existing) = arango_revision {
        existing
    } else {
        tracing::info!(
            %revision_id,
            "revision missing from ArangoDB — self-healing from Postgres"
        );

        // Ensure the document shell exists in ArangoDB before writing the
        // revision — write_revision also upserts a document→revision edge
        // which requires the document node to be present.
        let document = content_repository::get_document_by_id(
            &state.persistence.postgres,
            revision_row.document_id,
        )
        .await
        .context("failed to load document for self-heal")?
        .with_context(|| format!("document {} not found in Postgres", revision_row.document_id))?;

        if state
            .arango_document_store
            .get_document(revision_row.document_id)
            .await
            .context("failed to check document in ArangoDB")?
            .is_none()
        {
            state
                .canonical_services
                .knowledge
                .create_document_shell(
                    state,
                    crate::services::knowledge::service::CreateKnowledgeDocumentCommand {
                        document_id: document.id,
                        workspace_id: document.workspace_id,
                        library_id: document.library_id,
                        external_key: document.external_key.clone(),
                        file_name: Some(document.external_key.clone()),
                        title: None,
                        document_state: document.document_state.clone(),
                    },
                )
                .await
                .with_context(|| {
                    format!("failed to self-heal knowledge document {} in ArangoDB", document.id)
                })?;
        }

        let cmd = crate::services::knowledge::service::CreateKnowledgeRevisionCommand {
            revision_id: revision_row.id,
            workspace_id: revision_row.workspace_id,
            library_id: revision_row.library_id,
            document_id: revision_row.document_id,
            revision_number: i64::from(revision_row.revision_number),
            revision_state: "accepted".to_string(),
            revision_kind: revision_row.content_source_kind.clone(),
            storage_ref: revision_row.storage_key.clone(),
            source_uri: revision_row.source_uri.clone(),
            document_hint: revision_row.document_hint.clone(),
            mime_type: revision_row.mime_type.clone(),
            checksum: revision_row.checksum.clone(),
            byte_size: revision_row.byte_size,
            title: revision_row.title.clone(),
            normalized_text: None,
            text_checksum: None,
            text_state: "accepted".to_string(),
            vector_state: "accepted".to_string(),
            graph_state: "accepted".to_string(),
            text_readable_at: None,
            vector_ready_at: None,
            graph_ready_at: None,
            superseded_by_revision_id: None,
        };
        state.canonical_services.knowledge.write_revision(state, cmd).await.with_context(|| {
            format!("failed to self-heal knowledge revision {revision_id} in ArangoDB")
        })?;
        // Re-read from ArangoDB so we have the canonical row.
        state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .context("failed to load self-healed revision from arango")?
            .with_context(|| {
                format!("self-healed revision {revision_id} was not persisted to arango")
            })?
    };

    let extracted_content = match resolve_canonical_extract_content(
        state, job, attempt_id, &revision,
    )
    .await
    {
        Ok(content) => content,
        Err(error) => {
            let failure_message = error.to_string();
            let failure_code = error.failure_code.clone();
            let elapsed_ms = Some(extract_content_start.elapsed().as_millis() as i64);
            if let Err(e) = state
                .canonical_services
                .knowledge
                .set_revision_extract_state(state, revision_id, "failed", None, None)
                .await
            {
                tracing::warn!(%revision_id, ?e, "failed to set revision extract state to failed");
            }
            if let Err(e) = state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                        stage_state: "failed".to_string(),
                        message: Some(failure_message),
                        details_json: serde_json::json!({
                            "failureCode": failure_code,
                        }),
                        provider_kind: None,
                        model_name: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: None,
                        estimated_cost: None,
                        currency_code: None,
                        elapsed_ms,
                    },
                )
                .await
            {
                tracing::warn!(%attempt_id, ?e, "failed to record extract_content stage failure event");
            }
            return Err(anyhow::Error::new(error));
        }
    };
    let normalized_text =
        extracted_content.extraction_plan.normalized_text.clone().unwrap_or_default();

    let text_checksum = {
        let mut hasher = Sha256::new();
        hasher.update(normalized_text.as_bytes());
        hex::encode(hasher.finalize())
    };

    state
        .canonical_services
        .knowledge
        .set_revision_extract_state(
            state,
            revision_id,
            "ready",
            Some(&normalized_text),
            Some(&text_checksum),
        )
        .await
        .context("failed to persist extracted content")?;

    // Persist image_checksum as a supplementary field on the Arango revision document.
    // Fire-and-forget: a write failure is non-fatal (worst case: chunk reuse skipped on next revision).
    if let Some(ref checksum) = extracted_content.extraction_plan.image_checksum {
        if let Err(e) = state
            .arango_document_store
            .update_revision_image_checksum(revision_id, Some(checksum.as_str()))
            .await
        {
            tracing::warn!(%revision_id, ?e, "failed to persist image_checksum");
        }
    }

    let extract_content_elapsed_ms = Some(extract_content_start.elapsed().as_millis() as i64);

    // Capture vision billing if LLM was used for content extraction
    if extracted_content.provider_kind.is_some() {
        if let Err(e) = state
            .canonical_services
            .billing
            .capture_ingest_attempt(
                state,
                crate::services::ops::billing::CaptureIngestAttemptBillingCommand {
                    workspace_id: job.workspace_id,
                    library_id: job.library_id,
                    attempt_id,
                    binding_id: None,
                    provider_kind: extracted_content.provider_kind.clone().unwrap_or_default(),
                    model_name: extracted_content.model_name.clone().unwrap_or_default(),
                    call_kind: "vision_extract".to_string(),
                    usage_json: extracted_content.usage_json.clone(),
                },
            )
            .await
        {
            warn!(%worker_id, job_id = %job.id, ?e, "vision billing capture failed");
        }
    }

    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                stage_state: "completed".to_string(),
                message: Some("content extracted".to_string()),
                details_json: extracted_content.stage_details,
                provider_kind: extracted_content.provider_kind.clone(),
                model_name: extracted_content.model_name.clone(),
                prompt_tokens: extracted_content
                    .usage_json
                    .get("prompt_tokens")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                completion_tokens: extracted_content
                    .usage_json
                    .get("completion_tokens")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                total_tokens: extracted_content
                    .usage_json
                    .get("total_tokens")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: extract_content_elapsed_ms,
            },
        )
        .await
        .context("failed to record extract_content stage event")?;

    // --- Stage: prepare_structure / chunk_content / extract_technical_facts ---
    check_job_cancellation(
        cancellation_token,
        user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
        job.id,
    )?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_PREPARE_STRUCTURE.to_string(),
                stage_state: "started".to_string(),
                message: Some("building structured revision from normalized text".to_string()),
                details_json: serde_json::json!({
                    "libraryId": revision.library_id,
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
        .context("failed to record prepare_structure start stage event")?;

    let prepare_structure_start = Instant::now();
    let preparation = match state
        .canonical_services
        .content
        .prepare_and_persist_revision_structure(
            state,
            revision_id,
            &extracted_content.extraction_plan,
            cancellation_token,
        )
        .await
    {
        Ok(preparation) => preparation,
        Err(error) => {
            let mapped_error = map_stage_error(
                error.into(),
                user_cancel_requested,
                operator_pause_requested,
                lease_lost_requested,
                job.id,
                "failed to prepare and persist structured revision",
            );
            let failure_message = format!("{mapped_error:#}");
            let elapsed_ms = Some(prepare_structure_start.elapsed().as_millis() as i64);
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_PREPARE_STRUCTURE.to_string(),
                        stage_state: "failed".to_string(),
                        message: Some("structured revision preparation failed".to_string()),
                        details_json: serde_json::json!({
                            "revisionId": revision_id,
                            "error": failure_message,
                        }),
                        provider_kind: None,
                        model_name: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        cached_tokens: None,
                        estimated_cost: None,
                        currency_code: None,
                        elapsed_ms,
                    },
                )
                .await
                .context("failed to record prepare_structure failure stage event")?;
            return Err(mapped_error);
        }
    };

    let prepare_structure_elapsed_ms = Some(preparation.prepare_structure_elapsed_ms);
    let chunk_content_elapsed_ms = Some(preparation.chunk_content_elapsed_ms);
    let extract_technical_facts_elapsed_ms = Some(preparation.extract_technical_facts_elapsed_ms);

    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_PREPARE_STRUCTURE.to_string(),
                stage_state: "completed".to_string(),
                message: Some("structured revision prepared".to_string()),
                details_json: serde_json::json!({
                    "revisionId": revision_id,
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
                elapsed_ms: prepare_structure_elapsed_ms,
            },
        )
        .await
        .context("failed to record prepare_structure stage event")?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_CHUNK_CONTENT.to_string(),
                stage_state: "started".to_string(),
                message: None,
                details_json: serde_json::json!({}),
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
        .context("failed to record chunk_content started stage event")?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_CHUNK_CONTENT.to_string(),
                stage_state: "completed".to_string(),
                message: Some("content chunks persisted".to_string()),
                details_json: serde_json::json!({
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
                elapsed_ms: chunk_content_elapsed_ms,
            },
        )
        .await
        .context("failed to record chunk_content stage event")?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS.to_string(),
                stage_state: "started".to_string(),
                message: None,
                details_json: serde_json::json!({}),
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
        .context("failed to record extract_technical_facts started stage event")?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS.to_string(),
                stage_state: "completed".to_string(),
                message: Some("technical facts extracted from structured revision".to_string()),
                details_json: serde_json::json!({
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
                elapsed_ms: extract_technical_facts_elapsed_ms,
            },
        )
        .await
        .context("failed to record extract_technical_facts stage event")?;
    drop(extracted_content.extraction_plan);
    if heavy_revision_pipeline_limited {
        drop(heavy_revision_pipeline_permit);
        tracing::info!(
            %revision_id,
            "heavy revision pipeline slot released before provider-bound stages"
        );
    }

    // --- Stage: embed_chunk ---------------------------------------------------
    // Chunk embedding is required for a readable revision. Failure marks
    // vector/graph readiness failed and aborts this attempt; the worker does
    // not continue into graph extraction with a partial vector inventory.
    check_job_cancellation(
        cancellation_token,
        user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
        job.id,
    )?;
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
        .context("failed to record embed_chunk started stage event")?;

    let embed_chunk_start = Instant::now();
    let embed_chunk_outcome = state
        .canonical_services
        .search
        .embed_chunks_for_revision(
            state,
            revision.library_id,
            revision_id,
            Some(attempt_id),
            cancellation_token,
        )
        .await;
    let embed_chunk_elapsed_ms = Some(embed_chunk_start.elapsed().as_millis() as i64);
    let mut embed_chunk_failure: Option<String> = None;
    let embed_chunk_success = match &embed_chunk_outcome {
        Ok(outcome) => {
            if let (Some(provider), Some(model), Some(usage_json)) = (
                outcome.provider_kind.clone(),
                outcome.model_name.clone(),
                outcome.usage_json.clone(),
            ) {
                if let Err(e) = state
                    .canonical_services
                    .billing
                    .capture_ingest_attempt(
                        state,
                        crate::services::ops::billing::CaptureIngestAttemptBillingCommand {
                            workspace_id: job.workspace_id,
                            library_id: job.library_id,
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
                    warn!(%worker_id, job_id = %job.id, ?e, "embed_chunk billing capture failed");
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
                            "chunksReused": outcome.chunks_reused,
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
                .await
                .context("failed to record embed_chunk stage event")?;
            true
        }
        Err(error) => {
            if matches!(error, crate::services::query::error::QueryServiceError::Cancelled) {
                return Err(job_cancellation_error(
                    job.id,
                    user_cancel_requested,
                    operator_pause_requested,
                    lease_lost_requested,
                ));
            }
            embed_chunk_failure = Some(format!("chunk embedding failed: {error:#}"));
            warn!(
                %worker_id,
                job_id = %job.id,
                revision_id = %revision_id,
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
                .await
                .context("failed to record embed_chunk failed stage event")?;
            false
        }
    };
    drop(embed_chunk_outcome);
    if let Some(reason) = embed_chunk_failure {
        fail_revision_vector_graph_readiness(state, revision_id, &reason).await?;
        return Err(anyhow::anyhow!(reason));
    }

    // --- Stage: extract_graph -------------------------------------------------
    check_job_cancellation(
        cancellation_token,
        user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
        job.id,
    )?;
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
                    "libraryId": revision.library_id,
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
        .context("failed to record extract_graph start stage event")?;

    let extract_graph_start = Instant::now();
    // Graph candidate materialization is a per-chunk checkpointed stage:
    // each successful chunk writes a ready `runtime_graph_extraction` row
    // and provider calls carry their own request timeout. The content service
    // enforces an idle/progress timeout there; this worker only keeps the
    // bounded timeout for the final reconcile step, which is a single
    // graph-store projection operation.
    let graph_reconcile_timeout =
        Duration::from_secs(state.settings.runtime_graph_extract_stage_timeout_seconds.max(1));

    let graph_materialization = match state
        .canonical_services
        .content
        .materialize_revision_graph_candidates(
            state,
            MaterializeRevisionGraphCandidatesCommand {
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                revision_id,
                attempt_id: Some(attempt_id),
            },
            cancellation_token,
        )
        .await
    {
        Ok(materialization) => Ok(materialization),
        Err(crate::services::content::error::ContentServiceError::Cancelled) => {
            return Err(job_cancellation_error(
                job.id,
                user_cancel_requested,
                operator_pause_requested,
                lease_lost_requested,
            ));
        }
        Err(error) => Err(error),
    };
    let mut graph_ready = false;
    let mut graph_failure: Option<String> = None;

    match graph_materialization {
        Ok(graph_materialization) => {
            let graph_outcome = match time::timeout(
                graph_reconcile_timeout,
                state.canonical_services.graph.reconcile_revision_graph(
                    state,
                    job.library_id,
                    document_id,
                    revision_id,
                    Some(attempt_id),
                    cancellation_token,
                ),
            )
            .await
            {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(crate::services::graph::error::GraphServiceError::Cancelled)) => {
                    return Err(job_cancellation_error(
                        job.id,
                        user_cancel_requested,
                        operator_pause_requested,
                        lease_lost_requested,
                    ));
                }
                Ok(Err(error)) => Err(error),
                Err(_) => Err(crate::services::graph::error::GraphServiceError::StateConflict {
                    message: format!(
                        "extract_graph stage exceeded canonical timeout of {}s during revision graph reconcile",
                        graph_reconcile_timeout.as_secs()
                    ),
                }),
            };
            graph_ready = graph_outcome.as_ref().is_ok_and(|outcome| outcome.graph_ready);

            match graph_outcome {
                Ok(outcome) => {
                    let extract_graph_elapsed_ms =
                        Some(extract_graph_start.elapsed().as_millis() as i64);
                    state
                        .canonical_services
                        .ingest
                        .record_stage_event(
                            state,
                            RecordStageEventCommand {
                                attempt_id,
                                stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
                                stage_state: "completed".to_string(),
                                message: Some(graph_extract_success_message(graph_ready).to_string()),
                                details_json: serde_json::json!({
                                    "chunksProcessed": graph_materialization.chunk_count,
                                    "graphChunksSelected": graph_materialization.selected_graph_chunks,
                                    "recordStreamSourceUnitsSkipped": graph_materialization.record_stream_source_units_skipped,
                                    "extractedEntityCandidates": graph_materialization.extracted_entities,
                                    "extractedRelationCandidates": graph_materialization.extracted_relations,
                                    "reusedChunks": graph_materialization.reused_chunks,
                                    "reusedPromptHashMismatches": graph_materialization.reused_prompt_hash_mismatches,
                                    "reusedEntities": graph_materialization.reused_entities,
                                    "reusedRelations": graph_materialization.reused_relations,
                                    "projectedNodes": outcome.projection.node_count,
                                    "projectedEdges": outcome.projection.edge_count,
                                    "projectionVersion": outcome.projection.projection_version,
                                    "graphStatus": outcome.projection.graph_status,
                                    "graphContributionCount": outcome.graph_contribution_count,
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
                                elapsed_ms: extract_graph_elapsed_ms,
                            },
                        )
                        .await
                        .context("failed to record extract_graph stage event")?;
                }
                Err(graph_error) => {
                    graph_failure = Some(format!("graph reconcile failed: {graph_error:#}"));
                    warn!(
                        %worker_id,
                        job_id = %job.id,
                        revision_id = %revision_id,
                        ?graph_error,
                        "canonical graph rebuild failed",
                    );
                    let extract_graph_elapsed_ms =
                        Some(extract_graph_start.elapsed().as_millis() as i64);
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
                                    "graph rebuild failed".to_string(),
                                ),
                                details_json: serde_json::json!({
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
                                provider_kind: graph_materialization.provider_kind.clone(),
                                model_name: graph_materialization.model_name.clone(),
                                prompt_tokens: graph_materialization.usage_json.get("prompt_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                completion_tokens: graph_materialization.usage_json.get("completion_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                total_tokens: graph_materialization.usage_json.get("total_tokens").and_then(|v| v.as_i64()).map(|v| v as i32),
                                cached_tokens: None,
                                estimated_cost: None,
                                currency_code: None,
                                elapsed_ms: extract_graph_elapsed_ms,
                            },
                        )
                        .await
                        .context("failed to record extract_graph failure stage event")?;
                }
            }
        }
        Err(error) => {
            graph_failure = Some(format!("graph candidate extraction failed: {error:#}"));
            warn!(
                %worker_id,
                job_id = %job.id,
                revision_id = %revision_id,
                ?error,
                "graph candidate extraction failed",
            );
            let extract_graph_elapsed_ms = Some(extract_graph_start.elapsed().as_millis() as i64);
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
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
                    },
                )
                .await
                .context("failed to record extract_graph extraction failure stage event")?;
        }
    }

    if let Some(reason) = graph_failure {
        fail_revision_vector_graph_readiness(state, revision_id, &reason).await?;
        return Err(anyhow::anyhow!(reason));
    }

    // --- Generate document summary from structured blocks ---------------------
    match generate_document_summary_from_blocks(state, revision_id).await {
        Ok(summary) if !summary.is_empty() => {
            if let Err(error) = content_repository::update_document_summary(
                &state.persistence.postgres,
                document_id,
                &summary,
            )
            .await
            {
                tracing::warn!(document_id = %document_id, ?error, "failed to persist document summary");
            }
        }
        Err(error) => {
            tracing::warn!(document_id = %document_id, ?error, "failed to generate document summary");
        }
        _ => {}
    }

    // --- Stage: finalize readiness --------------------------------------------
    check_job_cancellation(
        cancellation_token,
        user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
        job.id,
    )?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_FINALIZING.to_string(),
                stage_state: "started".to_string(),
                message: None,
                details_json: serde_json::json!({}),
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
        .context("failed to record finalizing started stage event")?;

    let finalizing_start = Instant::now();

    let now = Utc::now();
    let vector_state_label = if embed_chunk_success { "ready" } else { "failed" };
    let vector_ready_at = embed_chunk_success.then_some(now);
    let _ = state
        .arango_document_store
        .update_revision_readiness(
            revision_id,
            "ready",
            vector_state_label,
            graph_state_after_successful_extract(graph_ready),
            Some(now),
            vector_ready_at,
            graph_ready.then_some(now),
            revision.superseded_by_revision_id,
        )
        .await
        .context("failed to update revision readiness")?;

    // Fail-loud finalize contract. The previous `if let Err(e) { warn!; }`
    // path silently swallowed mutation-state update failures while
    // `promote_document_head` ran regardless — the result was documents
    // with `readable_revision_id IS NOT NULL` on head but
    // `mutation_state` stuck in `accepted`/`running`, which then
    // diverged across the multiple dashboard aggregates and produced
    // the "920 ready" frozen-counter report.
    //
    // Now every finalize sub-step returns its error up the stack. If
    // any step fails, `?` bubbles out before `promote_document_head`
    // so the document head NEVER gains a readable revision out of sync
    // with its mutation. The attempt transitions to `failed` and the
    // job will be retried by the scheduler — a second pass either
    // completes atomically or stays in the failed bucket where
    // operators can see it.
    //
    // Not a Postgres `Transaction` yet because `promote_document_head`
    // writes to both Postgres and Arango and crossing databases inside
    // one `BEGIN` is a larger refactor (see
    // `services/content/service/revision.rs::promote_document_head`).
    // The fail-loud ordering gives us the same drift-prevention
    // guarantee for all future ingests without changing the executor
    // plumbing.
    if let Some(mutation_id) = job.mutation_id {
        let items =
            content_repository::list_mutation_items(&state.persistence.postgres, mutation_id)
                .await
                .context("failed to list mutation items during finalize")?;
        if let Some(item) = items.first() {
            content_repository::update_mutation_item(
                &state.persistence.postgres,
                item.id,
                Some(document_id),
                item.base_revision_id,
                Some(revision_id),
                "applied",
                Some("mutation applied by canonical worker"),
            )
            .await
            .with_context(|| {
                format!(
                    "failed to update mutation item to applied (mutation_id={mutation_id}, item_id={})",
                    item.id
                )
            })?;
        }
        content_repository::update_mutation_status(
            &state.persistence.postgres,
            mutation_id,
            "applied",
            Some(Utc::now()),
            None,
            None,
        )
        .await
        .with_context(|| {
            format!("failed to update mutation status to applied (mutation_id={mutation_id})")
        })?;
    }

    // Promote the document head through the canonical service so
    // Postgres and Arango stay aligned. Runs AFTER mutation updates
    // succeed — any earlier error above has already bubbled out and
    // prevented the head from reaching the readable-revision state.
    state
        .canonical_services
        .content
        .promote_document_head(
            state,
            PromoteHeadCommand {
                document_id,
                active_revision_id: Some(revision_id),
                readable_revision_id: Some(revision_id),
                latest_mutation_id: job.mutation_id,
                latest_successful_attempt_id: Some(attempt_id),
            },
        )
        .await
        .with_context(|| {
            format!(
                "failed to promote document head (document_id={document_id}, revision_id={revision_id})"
            )
        })?;
    state
        .canonical_services
        .content
        .converge_document_technical_facts(state, document_id, Some(revision_id))
        .await
        .context("failed to converge typed technical facts for current revision")?;

    // Fire-and-forget outbound webhook fanout for `revision.ready`.
    // Errors are logged at WARN level and do NOT fail the ingest job.
    {
        let event = crate::domains::webhook::WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: format!("revision.ready:{}:{}", revision_id, uuid::Uuid::now_v7()),
            workspace_id: job.workspace_id,
            library_id: Some(job.library_id),
            payload_json: serde_json::json!({
                "document_id": document_id,
                "revision_id": revision_id,
                "library_id": job.library_id,
            }),
        };
        let errors = crate::services::webhook::outbound::publish_webhook_event(
            &state.persistence.postgres,
            &event,
        )
        .await;
        for err in &errors {
            warn!(
                %document_id,
                %revision_id,
                error = %err,
                "outbound webhook publish error after promote_document_head"
            );
        }
    }

    let finalizing_elapsed_ms = Some(finalizing_start.elapsed().as_millis() as i64);

    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_FINALIZING.to_string(),
                stage_state: "completed".to_string(),
                message: Some("canonical ingest pipeline completed".to_string()),
                details_json: serde_json::json!({
                    "revisionId": revision_id,
                    "documentId": document_id,
                }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: finalizing_elapsed_ms,
            },
        )
        .await
        .context("failed to record finalizing stage event")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mib(value: u64) -> u64 {
        value * 1024 * 1024
    }

    #[test]
    fn auto_heavy_pipeline_parallelism_uses_cpu_memory_and_default_cap() {
        assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(6, Some(mib(8192)), 2), 4);
        assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(2, Some(mib(8192)), 4), 2);
        assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, Some(mib(6144)), 4), 3);
        assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, Some(mib(4096)), 4), 1);
        assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, Some(mib(8192)), 1), 2);
        assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(8, None, 4), 1);
        assert_eq!(auto_heavy_revision_pipeline_parallelism_for_limits(0, Some(mib(8192)), 4), 1);
    }
}
