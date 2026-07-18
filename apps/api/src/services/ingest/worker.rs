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
    infra::repositories::{catalog_repository, content_repository, ingest_repository},
    integrations::docling,
    interfaces::http::router_support::ApiError,
    services::{
        content::service::{
            GRAPH_STATE_DEGRADED, MaterializeRevisionGraphCandidatesCommand,
            RevisionGraphCandidateMaterialization, graph_extract_success_message,
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
        query::vector_dimensions::{
            ensure_active_embedding_profile_key, invalidate_library_embedding_profile_inventory,
        },
        webhook::error::WebhookServiceError,
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
    const fn new(running: Arc<AtomicBool>) -> Self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CanonicalContentPipelineOutcome {
    LifecyclePublished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CanonicalGraphRefreshOutcome {
    LifecyclePublished,
}

/// Carries structural cleanup intent across the pipeline boundary without
/// inferring it from provider text. Only lifecycle invalidation after a
/// successful vector write requests revision-wide deletion.
#[derive(Debug, Error)]
#[error("{source:#}")]
struct CanonicalContentPipelineFailure {
    delete_vectors: bool,
    #[source]
    source: anyhow::Error,
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
        Some(value) => {
            if let Some(value) = value.parse::<usize>().ok().filter(|value| *value > 0) {
                tracing::info!(
                    parallelism = value,
                    "heavy revision pipeline parallelism configured"
                );
                value
            } else {
                tracing::warn!(
                    raw = value,
                    fallback_parallelism = 1,
                    "invalid IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM; using fail-safe heavy revision pipeline parallelism"
                );
                1
            }
        }
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
    let memory_bound =
        memory_limit_bytes.map(|bytes| bytes / (1024 * 1024)).map_or(1, |memory_mib| {
            let soft_limit_mib = memory_mib.saturating_mul(9) / 10;
            soft_limit_mib
                .saturating_sub(HEAVY_PIPELINE_AUTO_RESERVED_MEMORY_MIB)
                .checked_div(HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB)
                .unwrap_or(0) as usize
        });

    cpu_bound
        .min(docling_bound)
        .min(memory_bound.max(1))
        .clamp(1, HEAVY_PIPELINE_AUTO_MAX_PARALLELISM)
}

fn vision_billing_usage_items(usage_json: &serde_json::Value) -> Vec<serde_json::Value> {
    usage_json
        .get("embedded_picture_ocr_usage")
        .and_then(serde_json::Value::as_array)
        .filter(|items| !items.is_empty())
        .map_or_else(|| vec![usage_json.clone()], Clone::clone)
}

fn graph_stage_event_command(
    attempt_id: Uuid,
    materialization: &RevisionGraphCandidateMaterialization,
    stage_state: &str,
    message: &str,
    details_json: serde_json::Value,
    elapsed_ms: Option<i64>,
) -> RecordStageEventCommand {
    RecordStageEventCommand {
        attempt_id,
        stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
        stage_state: stage_state.to_string(),
        message: Some(message.to_string()),
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

fn graph_usage_token_count(usage_json: &serde_json::Value, field: &str) -> Option<i32> {
    usage_json
        .get(field)
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
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

    pub(super) fn extraction_failed_terminal(
        failure_code: &str,
        message: impl std::fmt::Display,
    ) -> Self {
        Self {
            failure_code: failure_code.to_string(),
            retryable: false,
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

struct CanonicalPipelineCancellation<'a> {
    token: &'a CancellationToken,
    user_requested: &'a AtomicBool,
    operator_pause_requested: &'a AtomicBool,
    lease_lost_requested: &'a AtomicBool,
}

impl CanonicalPipelineCancellation<'_> {
    fn check(&self, job_id: Uuid) -> anyhow::Result<()> {
        check_job_cancellation(
            self.token,
            self.user_requested,
            self.operator_pause_requested,
            self.lease_lost_requested,
            job_id,
        )
    }

    fn error(&self, job_id: Uuid) -> anyhow::Error {
        job_cancellation_error(
            job_id,
            self.user_requested,
            self.operator_pause_requested,
            self.lease_lost_requested,
        )
    }
}

struct CanonicalPreparedContent {
    revision: crate::infra::knowledge_rows::KnowledgeRevisionRow,
}

struct CanonicalEmbeddingOutcome {
    source_truth_version: i64,
    embedding_profile_key: Option<String>,
}

struct CanonicalGraphStageOutcome {
    graph_ready: bool,
    graph_degraded: bool,
    pending_summary_refresh: Option<crate::services::graph::summary::PendingGraphSummaryRefresh>,
}

#[derive(Default)]
struct CanonicalDispatchOutcome {
    content_ingest_finalized: bool,
    graph_refresh_finalized: bool,
    webhook_ingest_finalized: bool,
}

impl CanonicalDispatchOutcome {
    const fn is_finalized(&self) -> bool {
        self.content_ingest_finalized
            || self.graph_refresh_finalized
            || self.webhook_ingest_finalized
    }
}

struct CanonicalAttemptLifecycle<'a> {
    state: &'a Arc<AppState>,
    worker_id: &'a str,
    job: &'a ingest_repository::IngestJobRow,
    attempt_id: Uuid,
    initial_stage: &'a str,
    expected_queue_lease_token: &'a str,
    cancellation: CanonicalPipelineCancellation<'a>,
}

#[must_use]
pub fn spawn_ingestion_worker(
    state: AppState,
    shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_ingestion_worker_pool(Arc::new(state), shutdown).await;
    })
}

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

async fn observe_initial_queue_state(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
) -> anyhow::Result<()> {
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

async fn dispatch_content_mutation(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
) -> anyhow::Result<()> {
    let revision_id = lifecycle
        .job
        .knowledge_revision_id
        .context("canonical ingest job is missing knowledge_revision_id")?;
    let document_id = lifecycle
        .job
        .knowledge_document_id
        .context("canonical ingest job is missing knowledge_document_id")?;
    let document =
        content_repository::get_document_by_id(&lifecycle.state.persistence.postgres, document_id)
            .await
            .map_err(|_| anyhow::anyhow!("failed to load document"))?;
    if !document.as_ref().is_some_and(|document| document.document_state == "deleted") {
        run_canonical_ingest_pipeline(
            lifecycle.state,
            lifecycle.worker_id,
            lifecycle.job,
            lifecycle.attempt_id,
            document_id,
            revision_id,
            lifecycle.cancellation.token,
            lifecycle.cancellation.user_requested,
            lifecycle.cancellation.operator_pause_requested,
            lifecycle.cancellation.lease_lost_requested,
        )
        .await?;
        return Ok(());
    }
    if let Some(mutation_id) = lifecycle.job.mutation_id {
        lifecycle
            .state
            .canonical_services
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

async fn dispatch_graph_refresh(lifecycle: &CanonicalAttemptLifecycle<'_>) -> anyhow::Result<()> {
    let revision_id = lifecycle
        .job
        .knowledge_revision_id
        .context("graph-refresh job is missing knowledge_revision_id")?;
    let document_id = lifecycle
        .job
        .knowledge_document_id
        .context("graph-refresh job is missing knowledge_document_id")?;
    run_canonical_graph_refresh(
        lifecycle.state,
        lifecycle.worker_id,
        lifecycle.job,
        lifecycle.attempt_id,
        document_id,
        revision_id,
        lifecycle.cancellation.token,
        lifecycle.cancellation.user_requested,
        lifecycle.cancellation.operator_pause_requested,
        lifecycle.cancellation.lease_lost_requested,
    )
    .await?;
    Ok(())
}

async fn dispatch_webhook_delivery(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
) -> anyhow::Result<bool> {
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

async fn dispatch_canonical_ingest_job(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
) -> anyhow::Result<CanonicalDispatchOutcome> {
    observe_initial_queue_state(lifecycle).await?;
    lifecycle.cancellation.check(lifecycle.job.id)?;
    let mut outcome = CanonicalDispatchOutcome::default();
    match lifecycle.job.job_kind.as_str() {
        "content_mutation" => {
            dispatch_content_mutation(lifecycle).await?;
            outcome.content_ingest_finalized = true;
        }
        "graph_refresh" => {
            dispatch_graph_refresh(lifecycle).await?;
            outcome.graph_refresh_finalized = true;
        }
        "web_discovery" => {
            Box::pin(run_canonical_web_discovery_job(
                lifecycle.state,
                lifecycle.job,
                lifecycle.attempt_id,
            ))
            .await?;
        }
        "web_materialize_page" => {
            run_canonical_web_materialize_page_job(
                lifecycle.state,
                lifecycle.job,
                lifecycle.attempt_id,
            )
            .await?;
        }
        "webhook_delivery" => {
            outcome.webhook_ingest_finalized = dispatch_webhook_delivery(lifecycle).await?;
        }
        other => anyhow::bail!("unsupported canonical ingest job kind {other}"),
    }
    Ok(outcome)
}

async fn finalize_successful_canonical_job(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
) -> anyhow::Result<()> {
    let final_stage = match lifecycle.job.job_kind.as_str() {
        "content_mutation" | "graph_refresh" => INGEST_STAGE_FINALIZING,
        "web_discovery" => INGEST_STAGE_WEB_DISCOVERY,
        "web_materialize_page" => INGEST_STAGE_WEB_MATERIALIZE_PAGE,
        "webhook_delivery" => INGEST_STAGE_WEBHOOK_DELIVERY,
        _ => lifecycle.initial_stage,
    };
    let result = lifecycle
        .state
        .canonical_services
        .ingest
        .finalize_attempt(
            lifecycle.state,
            FinalizeAttemptCommand {
                attempt_id: lifecycle.attempt_id,
                knowledge_generation_id: None,
                attempt_state: "succeeded".to_string(),
                current_stage: Some(final_stage.to_string()),
                failure_class: None,
                failure_code: None,
                failure_message: None,
                retryable: false,
            },
        )
        .await;
    let Err(error) = result else {
        info!(
            worker_id = %lifecycle.worker_id,
            job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id,
            "canonical ingest job completed",
        );
        return Ok(());
    };
    if !matches!(error, ApiError::Conflict(_)) {
        return Err(error).context("failed to finalize canonical ingest attempt as succeeded");
    }
    match ingest_repository::get_ingest_job_by_id(
        &lifecycle.state.persistence.postgres,
        lifecycle.job.id,
    )
    .await
    {
        Ok(Some(row)) if row.queue_state == "paused" => {
            if let Err(abandon_error) = ingest_repository::abandon_paused_ingest_attempt(
                &lifecycle.state.persistence.postgres,
                lifecycle.attempt_id,
            )
            .await
            {
                warn!(attempt_id = %lifecycle.attempt_id, ?abandon_error, "failed to finalize paused ingest attempt after successful pipeline return");
            }
            info!(
                worker_id = %lifecycle.worker_id,
                job_id = %lifecycle.job.id,
                attempt_id = %lifecycle.attempt_id,
                "canonical ingest job completed after operator pause; preserving paused queue state"
            );
            return Ok(());
        }
        Err(reload_error) => {
            warn!(attempt_id = %lifecycle.attempt_id, ?reload_error, "failed to reload ingest job after finalize conflict");
        }
        Ok(_) => {}
    }
    warn!(
        worker_id = %lifecycle.worker_id,
        job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        ?error,
        "canonical ingest job finished after losing its active lease; leaving queue state to the current owner"
    );
    Ok(())
}

async fn handle_cooperative_failure(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
    error: &anyhow::Error,
) -> anyhow::Result<bool> {
    if error.downcast_ref::<JobCanceledByRequest>().is_some() {
        info!(
            worker_id = %lifecycle.worker_id,
            job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id,
            "canonical ingest job observed user cancel request and stopped cooperatively",
        );
        return Ok(true);
    }
    if error.downcast_ref::<JobPausedByOperator>().is_some() {
        if let Err(abandon_error) = ingest_repository::abandon_paused_ingest_attempt(
            &lifecycle.state.persistence.postgres,
            lifecycle.attempt_id,
        )
        .await
        {
            warn!(attempt_id = %lifecycle.attempt_id, ?abandon_error, "failed to finalize paused ingest attempt");
        }
        info!(
            worker_id = %lifecycle.worker_id,
            job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id,
            "canonical ingest job observed operator pause request and stopped cooperatively",
        );
        return Ok(true);
    }
    if error.downcast_ref::<JobLeaseLost>().is_some() {
        warn!(
            worker_id = %lifecycle.worker_id,
            job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id,
            "canonical ingest job stopped because its attempt lease was lost"
        );
        return Ok(true);
    }
    Ok(false)
}

async fn defer_in_flight_webhook_failure(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
    error: &anyhow::Error,
) -> bool {
    let Some(WebhookServiceError::DeliveryLeaseInFlight { retry_at, .. }) =
        error.downcast_ref::<WebhookServiceError>()
    else {
        return false;
    };
    match ingest_repository::defer_webhook_delivery_in_flight(
        &lifecycle.state.persistence.postgres,
        lifecycle.attempt_id,
        lifecycle.job.id,
        lifecycle.expected_queue_lease_token,
        *retry_at,
    )
    .await
    {
        Ok(true) => info!(
            worker_id = %lifecycle.worker_id,
            job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id,
            retry_at = %retry_at,
            "deferred duplicate webhook job until the current delivery lease expires"
        ),
        Ok(false) => warn!(
            worker_id = %lifecycle.worker_id,
            job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id,
            "webhook job lease moved before in-flight delivery deferral"
        ),
        Err(defer_error) => {
            warn!(
                worker_id = %lifecycle.worker_id,
                job_id = %lifecycle.job.id,
                attempt_id = %lifecycle.attempt_id,
                ?defer_error,
                "failed to defer duplicate webhook job; using ordinary retry path"
            );
            return false;
        }
    }
    true
}

async fn finalize_special_failure(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
    error: &anyhow::Error,
) -> anyhow::Result<bool> {
    if error.downcast_ref::<JobCanceledByShutdown>().is_some() {
        let current_job = ingest_repository::get_ingest_job_by_id(
            &lifecycle.state.persistence.postgres,
            lifecycle.job.id,
        )
        .await;
        if current_job
            .as_ref()
            .is_ok_and(|row| row.as_ref().is_some_and(|row| row.queue_state == "canceled"))
        {
            info!(
                worker_id = %lifecycle.worker_id,
                job_id = %lifecycle.job.id,
                attempt_id = %lifecycle.attempt_id,
                "canonical ingest job stopped during shutdown after user cancel won the race",
            );
            return Ok(true);
        }
        if let Err(reload_error) = current_job {
            warn!(attempt_id = %lifecycle.attempt_id, ?reload_error, "failed to reload ingest job while finalizing shutdown cancellation");
        }
        if let Err(finalize_error) = lifecycle
            .state
            .canonical_services
            .ingest
            .finalize_attempt(
                lifecycle.state,
                FinalizeAttemptCommand {
                    attempt_id: lifecycle.attempt_id,
                    knowledge_generation_id: None,
                    attempt_state: "failed".to_string(),
                    current_stage: Some(lifecycle.initial_stage.to_string()),
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
            warn!(attempt_id = %lifecycle.attempt_id, ?finalize_error, "failed to requeue shutdown-canceled attempt");
        }
        info!(
            worker_id = %lifecycle.worker_id,
            job_id = %lifecycle.job.id,
            attempt_id = %lifecycle.attempt_id,
            "canonical ingest job stopped cooperatively for worker shutdown",
        );
        return Ok(true);
    }
    if error.downcast_ref::<DeletedDocumentJobSkipped>().is_none() {
        return Ok(false);
    }
    if let Err(finalize_error) = lifecycle
        .state
        .canonical_services
        .ingest
        .finalize_attempt(
            lifecycle.state,
            FinalizeAttemptCommand {
                attempt_id: lifecycle.attempt_id,
                knowledge_generation_id: None,
                attempt_state: "canceled".to_string(),
                current_stage: Some(lifecycle.initial_stage.to_string()),
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
        warn!(attempt_id = %lifecycle.attempt_id, ?finalize_error, "failed to finalize deleted-document attempt as canceled");
    }
    info!(
        worker_id = %lifecycle.worker_id,
        job_id = %lifecycle.job.id,
        attempt_id = %lifecycle.attempt_id,
        "canonical ingest job canceled because document was deleted"
    );
    Ok(true)
}

async fn publish_content_ingest_failure(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
    error: anyhow::Error,
) -> anyhow::Result<()> {
    let job = lifecycle.job;
    let (Some(mutation_id), Some(mutation_item_id), Some(document_id), Some(revision_id)) = (
        job.mutation_id,
        job.mutation_item_id,
        job.knowledge_document_id,
        job.knowledge_revision_id,
    ) else {
        warn!(
            worker_id = %lifecycle.worker_id,
            job_id = %job.id,
            attempt_id = %lifecycle.attempt_id,
            "content ingest job has an incomplete lifecycle identity; refusing partial failure publication",
        );
        return Err(error)
            .context("content ingest job has incomplete mutation/document/revision identity");
    };
    let message = format!("{error:#}");
    let extract_error = error.downcast_ref::<CanonicalExtractContentError>();
    let failure_class = if extract_error.is_some() { "content_extract" } else { "worker_error" };
    let outcome = ingest_repository::fail_content_ingest_attempt(
        &lifecycle.state.persistence.postgres,
        &ingest_repository::FailContentIngestAttempt {
            workspace_id: job.workspace_id,
            library_id: job.library_id,
            document_id,
            revision_id,
            mutation_id,
            mutation_item_id,
            attempt_id: lifecycle.attempt_id,
            current_stage: None,
            failure_class: Some(failure_class.to_string()),
            failure_code: Some(extract_error.map_or_else(
                || "canonical_pipeline_failed".to_string(),
                |failure| failure.failure_code.clone(),
            )),
            failure_message: Some(message.clone()),
            retryable: extract_error.is_none_or(|failure| failure.retryable),
            delete_vectors: error
                .downcast_ref::<CanonicalContentPipelineFailure>()
                .is_some_and(|failure| failure.delete_vectors),
            failed_at: Utc::now(),
        },
    )
    .await;
    match outcome {
        Ok(ingest_repository::FailContentIngestAttemptOutcome::Applied {
            deleted,
            retry_scheduled,
            mutation_failed,
            ..
        }) => {
            invalidate_library_embedding_profile_inventory(job.library_id);
            if !retry_scheduled
                && let Err(settlement_error) = lifecycle
                    .state
                    .canonical_services
                    .web_ingest
                    .settle_materialized_page_for_mutation_item(lifecycle.state, mutation_item_id)
                    .await
            {
                warn!(
                    job_id = %job.id,
                    attempt_id = %lifecycle.attempt_id,
                    %mutation_item_id,
                    ?settlement_error,
                    "terminal content failure committed but web page settlement failed; read-side reconciliation will retry",
                );
            }
            if retry_scheduled {
                info!(
                    worker_id = %lifecycle.worker_id,
                    job_id = %job.id,
                    attempt_id = %lifecycle.attempt_id,
                    deleted,
                    "canonical content ingest failed retryably and was atomically requeued",
                );
            } else {
                warn!(
                    worker_id = %lifecycle.worker_id,
                    job_id = %job.id,
                    attempt_id = %lifecycle.attempt_id,
                    deleted,
                    mutation_failed,
                    "canonical content ingest was atomically finalized as failed",
                );
            }
            Ok(())
        }
        Ok(ingest_repository::FailContentIngestAttemptOutcome::AuthorityLost { .. }) => {
            warn!(
                worker_id = %lifecycle.worker_id,
                job_id = %job.id,
                attempt_id = %lifecycle.attempt_id,
                "content ingest failure arrived after attempt authority moved; preserving current owner state",
            );
            Ok(())
        }
        Err(publication_error) => {
            warn!(
                worker_id = %lifecycle.worker_id,
                job_id = %job.id,
                attempt_id = %lifecycle.attempt_id,
                ?publication_error,
                "failed to atomically publish canonical content ingest failure",
            );
            Err(error).with_context(|| {
                format!(
                    "atomic content-ingest failure publication also failed: {publication_error:#}; original failure: {message}"
                )
            })
        }
    }
}

async fn finalize_failed_canonical_job(
    lifecycle: &CanonicalAttemptLifecycle<'_>,
    error: anyhow::Error,
) -> anyhow::Result<()> {
    if handle_cooperative_failure(lifecycle, &error).await?
        || defer_in_flight_webhook_failure(lifecycle, &error).await
        || finalize_special_failure(lifecycle, &error).await?
    {
        return Ok(());
    }
    if lifecycle.job.job_kind == "content_mutation" {
        return publish_content_ingest_failure(lifecycle, error).await;
    }
    let state = lifecycle.state;
    let worker_id = lifecycle.worker_id;
    let job = lifecycle.job;
    let attempt_id = lifecycle.attempt_id;
    let job_id = job.id;
    let initial_stage = lifecycle.initial_stage.to_string();
    let message = format!("{error:#}");
    let extract_error = error.downcast_ref::<CanonicalExtractContentError>();
    match state
        .canonical_services
        .ingest
        .finalize_attempt(
            state,
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
                        "graph_refresh" => "graph_refresh",
                        _ => "worker_error",
                    }
                    .to_string(),
                ),
                failure_code: Some(extract_error.map_or_else(
                    || match job.job_kind.as_str() {
                        "web_discovery" => "web_discovery_failed".to_string(),
                        "web_materialize_page" => "web_materialize_page_failed".to_string(),
                        "graph_refresh" => "graph_refresh_failed".to_string(),
                        _ => "canonical_pipeline_failed".to_string(),
                    },
                    |failure| failure.failure_code.clone(),
                )),
                failure_message: Some(message.clone()),
                retryable: extract_error.is_none_or(|failure| failure.retryable),
            },
        )
        .await
    {
        Ok(finalized) if finalized.retryable => {
            // `finalize_attempt` already requeued the job (queue_state ->
            // queued, available_at -> now + backoff) because the failure
            // was retryable and the attempt budget still has room. Return
            // Ok so the dispatcher's `handle_job_outcome` does NOT route
            // this through `fail_canonical_ingest_job`, which would clobber
            // the freshly requeued state back to terminal `failed` and kill
            // the retry. The next dispatcher tick re-leases the job once the
            // backoff window elapses.
            info!(
                %worker_id,
                %job_id,
                %attempt_id,
                attempt_number = finalized.attempt_number,
                "canonical ingest job failed retryably; requeued for another attempt",
            );
            Ok(())
        }
        Ok(_) => {
            // Terminal failure (non-retryable, or retry budget exhausted).
            // Propagate the error so the dispatcher reconciles the mutation
            // and marks the job permanently failed.
            Err(error).context(message)
        }
        Err(e) => {
            tracing::warn!(%attempt_id, ?e, "failed to finalize attempt as failed");
            Err(error).context(message)
        }
    }
}

async fn execute_canonical_ingest_job(
    state: Arc<AppState>,
    worker_id: &str,
    job: ingest_repository::IngestJobRow,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let job_id = job.id;
    let expected_queue_lease_token = job
        .queue_lease_token
        .clone()
        .context("claimed canonical ingest job is missing queue lease token")?;
    let initial_stage = match job.job_kind.as_str() {
        "content_mutation" => INGEST_STAGE_EXTRACT_CONTENT.to_string(),
        "graph_refresh" => INGEST_STAGE_EXTRACT_GRAPH.to_string(),
        "web_discovery" => INGEST_STAGE_WEB_DISCOVERY.to_string(),
        "web_materialize_page" => INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
        "webhook_delivery" => INGEST_STAGE_WEBHOOK_DELIVERY.to_string(),
        other => anyhow::bail!("unsupported canonical ingest job kind {other}"),
    };

    let attempt = match state
        .canonical_services
        .ingest
        .lease_attempt(
            &state,
            LeaseAttemptCommand {
                job_id,
                worker_principal_id: None,
                lease_token: Some(format!("worker-{worker_id}-{}", Uuid::now_v7())),
                expected_queue_lease_token: Some(expected_queue_lease_token.clone()),
                knowledge_generation_id: None,
                current_stage: Some(initial_stage.clone()),
            },
        )
        .await
    {
        Ok(attempt) => attempt,
        Err(ApiError::Conflict(message)) => {
            warn!(
                %worker_id,
                %job_id,
                %message,
                "queue lease moved before canonical ingest attempt creation",
            );
            return Ok(());
        }
        Err(error) => return Err(error).context("failed to lease canonical ingest attempt"),
    };

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

    let cancellation = CanonicalPipelineCancellation {
        token: &cancellation_token,
        user_requested: &user_cancel_requested,
        operator_pause_requested: &operator_pause_requested,
        lease_lost_requested: &lease_lost_requested,
    };
    let lifecycle = CanonicalAttemptLifecycle {
        state: &state,
        worker_id,
        job: &job,
        attempt_id,
        initial_stage: &initial_stage,
        expected_queue_lease_token: &expected_queue_lease_token,
        cancellation,
    };
    let dispatch_result = dispatch_canonical_ingest_job(&lifecycle).await;
    let already_finalized =
        dispatch_result.as_ref().is_ok_and(CanonicalDispatchOutcome::is_finalized);
    let result = dispatch_result.map(|_| ());
    drop(heartbeat_guard);

    if already_finalized && result.is_ok() {
        info!(
            %worker_id,
            %job_id,
            %attempt_id,
            "canonical job atomically finalized the current ingest lease"
        );
        return Ok(());
    }

    match result {
        Ok(()) => finalize_successful_canonical_job(&lifecycle).await,
        Err(error) => finalize_failed_canonical_job(&lifecycle, error).await,
    }
}

async fn run_canonical_graph_refresh(
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
) -> anyhow::Result<CanonicalGraphRefreshOutcome> {
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
    let materialization = match state
        .canonical_services
        .content
        .materialize_revision_graph_candidates(
            state,
            MaterializeRevisionGraphCandidatesCommand {
                workspace_id: job.workspace_id,
                library_id: job.library_id,
                revision_id,
                attempt_id: Some(attempt_id),
            },
            cancellation_token,
        )
        .await
    {
        Ok(materialization) => materialization,
        Err(crate::services::content::error::ContentServiceError::Cancelled) => {
            return Err(job_cancellation_error(
                job.id,
                user_cancel_requested,
                operator_pause_requested,
                lease_lost_requested,
            ));
        }
        Err(error) => return Err(error).context("graph-refresh candidate extraction failed"),
    };

    let reconcile_timeout =
        Duration::from_secs(state.settings.runtime_graph_extract_stage_timeout_seconds.max(1));
    let graph_outcome = match time::timeout(
        reconcile_timeout,
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
        Ok(Ok(outcome)) => outcome,
        Ok(Err(crate::services::graph::error::GraphServiceError::Cancelled)) => {
            return Err(job_cancellation_error(
                job.id,
                user_cancel_requested,
                operator_pause_requested,
                lease_lost_requested,
            ));
        }
        Ok(Err(error)) => return Err(error).context("graph-refresh reconcile failed"),
        Err(_) => anyhow::bail!(
            "graph-refresh reconcile exceeded canonical timeout of {}s",
            reconcile_timeout.as_secs()
        ),
    };
    let graph_ready = graph_outcome.graph_ready;

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
                    "chunksProcessed": materialization.chunk_count,
                    "graphChunksSelected": materialization.selected_graph_chunks,
                    "recordStreamSourceUnitsSkipped": materialization.record_stream_source_units_skipped,
                    "extractedEntityCandidates": materialization.extracted_entities,
                    "extractedRelationCandidates": materialization.extracted_relations,
                    "reusedChunks": materialization.reused_chunks,
                    "reusedPromptHashMismatches": materialization.reused_prompt_hash_mismatches,
                    "reusedEntities": materialization.reused_entities,
                    "reusedRelations": materialization.reused_relations,
                    "projectedNodes": graph_outcome.projection.node_count,
                    "projectedEdges": graph_outcome.projection.edge_count,
                    "projectionVersion": graph_outcome.projection.projection_version,
                    "graphStatus": graph_outcome.projection.graph_status,
                    "graphContributionCount": graph_outcome.graph_contribution_count,
                    "graphReady": graph_ready,
                    "providerKind": materialization.provider_kind,
                    "modelName": materialization.model_name,
                }),
                provider_kind: materialization.provider_kind.clone(),
                model_name: materialization.model_name.clone(),
                prompt_tokens: materialization
                    .usage_json
                    .get("prompt_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .map(|value| value as i32),
                completion_tokens: materialization
                    .usage_json
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .map(|value| value as i32),
                total_tokens: materialization
                    .usage_json
                    .get("total_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .map(|value| value as i32),
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: Some(started_at.elapsed().as_millis() as i64),
            },
        )
        .await
        .context("failed to record graph-refresh completion stage event")?;

    check_job_cancellation(
        cancellation_token,
        user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
        job.id,
    )?;
    let completed_at = Utc::now();
    let publication = ingest_repository::publish_graph_refresh_success(
        &state.persistence.postgres,
        &ingest_repository::PublishGraphRefreshSuccess {
            workspace_id: job.workspace_id,
            library_id: job.library_id,
            document_id,
            revision_id,
            attempt_id,
            graph_state: graph_state_after_successful_extract(graph_ready).to_string(),
            graph_ready_at: graph_ready.then_some(completed_at),
            completed_at,
        },
    )
    .await
    .context("failed to atomically publish graph-refresh success")?;

    match publication {
        ingest_repository::PublishGraphRefreshSuccessOutcome::Applied { source_truth_version } => {
            if let Some(pending_summary_refresh) = graph_outcome.pending_summary_refresh.as_ref()
                && let Err(error) = state
                    .canonical_services
                    .graph
                    .apply_published_summary_refresh(
                        state,
                        job.library_id,
                        source_truth_version,
                        pending_summary_refresh,
                    )
                    .await
            {
                warn!(
                    %worker_id,
                    job_id = %job.id,
                    %attempt_id,
                    %revision_id,
                    source_truth_version,
                    ?error,
                    "graph-refresh lifecycle committed but canonical summary refresh failed",
                );
            }
            info!(
                %worker_id,
                job_id = %job.id,
                %attempt_id,
                %revision_id,
                "graph-refresh maintenance was atomically published",
            );
        }
        ingest_repository::PublishGraphRefreshSuccessOutcome::Superseded { .. } => {
            info!(
                %worker_id,
                job_id = %job.id,
                %attempt_id,
                %revision_id,
                "graph-refresh maintenance was superseded by a newer readable revision",
            );
        }
        ingest_repository::PublishGraphRefreshSuccessOutcome::AuthorityLost { .. } => {
            warn!(
                %worker_id,
                job_id = %job.id,
                %attempt_id,
                %revision_id,
                "graph-refresh publication arrived after attempt authority moved",
            );
        }
    }

    Ok(CanonicalGraphRefreshOutcome::LifecyclePublished)
}

async fn load_or_self_heal_knowledge_revision(
    state: &AppState,
    revision_row: &content_repository::ContentRevisionRow,
) -> anyhow::Result<crate::infra::knowledge_rows::KnowledgeRevisionRow> {
    if let Some(revision) = state
        .document_store
        .get_revision(revision_row.id)
        .await
        .context("failed to load knowledge revision from store")?
    {
        return Ok(revision);
    }
    let revision_id = revision_row.id;
    tracing::info!(%revision_id, "revision missing from knowledge store — self-healing from Postgres");
    let document = content_repository::get_document_by_id(
        &state.persistence.postgres,
        revision_row.document_id,
    )
    .await
    .context("failed to load document for self-heal")?
    .with_context(|| format!("document {} not found in Postgres", revision_row.document_id))?;
    let document_exists = state
        .document_store
        .get_document(revision_row.document_id)
        .await
        .context("failed to check document in knowledge store")?
        .is_some();
    if !document_exists {
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
                format!("failed to self-heal knowledge document {} in store", document.id)
            })?;
    }
    state
        .canonical_services
        .knowledge
        .write_revision(
            state,
            crate::services::knowledge::service::CreateKnowledgeRevisionCommand {
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
            },
        )
        .await
        .with_context(|| {
            format!("failed to self-heal knowledge revision {revision_id} in store")
        })?;
    state
        .document_store
        .get_revision(revision_id)
        .await
        .context("failed to load self-healed revision from knowledge store")?
        .with_context(|| {
            format!("self-healed revision {revision_id} was not persisted to the knowledge store")
        })
}

async fn record_extract_content_failure(
    state: &AppState,
    attempt_id: Uuid,
    revision_id: Uuid,
    error: &CanonicalExtractContentError,
    elapsed_ms: i64,
) {
    if let Err(update_error) = state
        .canonical_services
        .knowledge
        .set_revision_extract_state(state, revision_id, "failed", None, None)
        .await
    {
        tracing::warn!(%revision_id, ?update_error, "failed to set revision extract state to failed");
    }
    if let Err(event_error) = state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EXTRACT_CONTENT.to_string(),
                stage_state: "failed".to_string(),
                message: Some(error.to_string()),
                details_json: serde_json::json!({ "failureCode": error.failure_code }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: Some(elapsed_ms),
            },
        )
        .await
    {
        tracing::warn!(%attempt_id, ?event_error, "failed to record extract_content stage failure event");
    }
}

async fn prepare_canonical_content(
    state: &AppState,
    worker_id: &str,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
    revision_id: Uuid,
    cancellation: &CanonicalPipelineCancellation<'_>,
) -> anyhow::Result<CanonicalPreparedContent> {
    // --- Stage: extract_content -----------------------------------------------
    cancellation.check(job.id)?;
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

    // Read revision metadata from Postgres — the canonical source of truth. If
    // the knowledge-plane row has not been materialized yet, populate it here so
    // the pipeline has a consistent view.
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

    let revision = load_or_self_heal_knowledge_revision(state, &revision_row).await?;

    let extracted_content =
        match resolve_canonical_extract_content(state, job, attempt_id, &revision).await {
            Ok(content) => content,
            Err(error) => {
                record_extract_content_failure(
                    state,
                    attempt_id,
                    revision_id,
                    &error,
                    extract_content_start.elapsed().as_millis() as i64,
                )
                .await;
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

    // Persist image_checksum as a supplementary field on the knowledge revision.
    // Fire-and-forget: a write failure is non-fatal (worst case: chunk reuse skipped on next revision).
    if let Some(ref checksum) = extracted_content.extraction_plan.image_checksum
        && let Err(e) = state
            .document_store
            .update_revision_image_checksum(revision_id, Some(checksum.as_str()))
            .await
    {
        tracing::warn!(%revision_id, ?e, "failed to persist image_checksum");
    }

    let extract_content_elapsed_ms = Some(extract_content_start.elapsed().as_millis() as i64);

    // Capture document-understanding billing if an LLM was used for extraction.
    if let Some(provider_kind) = extracted_content.provider_kind.clone() {
        let model_name = extracted_content.model_name.clone().unwrap_or_default();
        for usage_json in vision_billing_usage_items(&extracted_content.usage_json) {
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
                        provider_kind: provider_kind.clone(),
                        model_name: model_name.clone(),
                        call_kind: "vision_extract".to_string(),
                        usage_json,
                    },
                )
                .await
            {
                warn!(%worker_id, job_id = %job.id, ?e, "document-understanding billing capture failed");
            }
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
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| v as i32),
                completion_tokens: extracted_content
                    .usage_json
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| v as i32),
                total_tokens: extracted_content
                    .usage_json
                    .get("total_tokens")
                    .and_then(serde_json::Value::as_i64)
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
    cancellation.check(job.id)?;
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
            cancellation.token,
        )
        .await
    {
        Ok(preparation) => preparation,
        Err(error) => {
            let mapped_error = map_stage_error(
                error.into(),
                cancellation.user_requested,
                cancellation.operator_pause_requested,
                cancellation.lease_lost_requested,
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

    Ok(CanonicalPreparedContent { revision })
}

async fn embed_canonical_content(
    state: &AppState,
    worker_id: &str,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    revision_id: Uuid,
    cancellation: &CanonicalPipelineCancellation<'_>,
) -> anyhow::Result<CanonicalEmbeddingOutcome> {
    // --- Stage: embed_chunk ---------------------------------------------------
    // Chunk embedding is required for a readable revision. Failure marks
    // vector/graph readiness failed and aborts this attempt; the worker does
    // not continue into graph extraction with a partial vector inventory.
    cancellation.check(job.id)?;
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

    let vector_write_source_truth_version = catalog_repository::get_library_source_truth_version(
        &state.persistence.postgres,
        revision.library_id,
    )
    .await
    .context("failed to capture embed_chunk source-truth fence")?;
    let embed_chunk_start = Instant::now();
    let embed_chunk_outcome = state
        .canonical_services
        .search
        .embed_chunks_for_revision(
            state,
            revision.library_id,
            revision_id,
            attempt_id,
            vector_write_source_truth_version,
            cancellation.token,
        )
        .await;
    let embed_chunk_elapsed_ms = Some(embed_chunk_start.elapsed().as_millis() as i64);
    let mut embed_chunk_failure: Option<String> = None;
    match &embed_chunk_outcome {
        Ok(outcome) => {
            if let (Some(provider), Some(model), Some(usage_json)) = (
                outcome.provider_kind.clone(),
                outcome.model_name.clone(),
                outcome.usage_json.clone(),
            ) && let Err(e) = state
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
        }
        Err(error) => {
            if matches!(error, crate::services::query::error::QueryServiceError::Cancelled) {
                return Err(cancellation.error(job.id));
            }
            embed_chunk_failure = Some(format!("chunk embedding failed: {error:#}"));
            warn!(
                %worker_id,
                job_id = %job.id,
                revision_id = %revision_id,
                ?error,
                "chunk embedding failed; readiness remains count-gated for retry",
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
        }
    };
    let embed_chunk_profile_key =
        embed_chunk_outcome.as_ref().ok().and_then(|outcome| outcome.embedding_profile_key.clone());
    drop(embed_chunk_outcome);
    if let Some(reason) = embed_chunk_failure {
        // The embedding service owns exact attempt-ID cleanup. The outer
        // lifecycle boundary atomically publishes failed readiness together
        // with the attempt/job transition.
        return Err(anyhow::anyhow!(reason));
    }
    Ok(CanonicalEmbeddingOutcome {
        source_truth_version: vector_write_source_truth_version,
        embedding_profile_key: embed_chunk_profile_key,
    })
}

async fn extract_canonical_graph(
    state: &AppState,
    worker_id: &str,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
    document_id: Uuid,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    revision_id: Uuid,
    cancellation: &CanonicalPipelineCancellation<'_>,
) -> anyhow::Result<CanonicalGraphStageOutcome> {
    // --- Stage: extract_graph -------------------------------------------------
    cancellation.check(job.id)?;
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
            cancellation.token,
        )
        .await
    {
        Ok(materialization) => Ok(materialization),
        Err(crate::services::content::error::ContentServiceError::Cancelled) => {
            return Err(cancellation.error(job.id));
        }
        Err(error) => Err(error),
    };
    let mut graph_ready = false;
    let mut graph_failure: Option<String> = None;
    let mut pending_summary_refresh = None;

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
                    cancellation.token,
                ),
            )
            .await
            {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(crate::services::graph::error::GraphServiceError::Cancelled)) => {
                    return Err(cancellation.error(job.id));
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
                    pending_summary_refresh = outcome.pending_summary_refresh.clone();
                    let extract_graph_elapsed_ms =
                        Some(extract_graph_start.elapsed().as_millis() as i64);
                    state
                        .canonical_services
                        .ingest
                        .record_stage_event(
                            state,
                            graph_stage_event_command(
                                attempt_id,
                                &graph_materialization,
                                "completed",
                                graph_extract_success_message(graph_ready),
                                serde_json::json!({
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
                                extract_graph_elapsed_ms,
                            ),
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
                            graph_stage_event_command(
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

    // Graph extraction is an enrichment layer over chunk-vector retrieval, not
    // the core. When the chunks for this revision are already embedded, a
    // terminal graph-extraction failure must NOT discard those vectors or block
    // the document from becoming searchable. Mark the graph layer degraded, keep
    // the vectors, promote the document as readable, and let the idle graph
    // re-extract loop backfill the graph on a later tick. Only when the vectors
    // themselves are missing do we fail destructively (nothing to preserve).
    let graph_degraded = graph_failure.is_some();
    if let Some(reason) = graph_failure {
        warn!(
            %worker_id,
            job_id = %job.id,
            revision_id = %revision_id,
            reason = %reason,
            "graph extraction degraded after provider retries; keeping embedded chunk vectors and promoting document as searchable (graph backfilled by idle re-extract loop)",
        );
    }
    Ok(CanonicalGraphStageOutcome { graph_ready, graph_degraded, pending_summary_refresh })
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
) -> anyhow::Result<CanonicalContentPipelineOutcome> {
    let cancellation = CanonicalPipelineCancellation {
        token: cancellation_token,
        user_requested: user_cancel_requested,
        operator_pause_requested,
        lease_lost_requested,
    };
    let CanonicalPreparedContent { revision } =
        prepare_canonical_content(state, worker_id, job, attempt_id, revision_id, &cancellation)
            .await?;

    let CanonicalEmbeddingOutcome {
        source_truth_version: vector_write_source_truth_version,
        embedding_profile_key: embed_chunk_profile_key,
    } = embed_canonical_content(
        state,
        worker_id,
        job,
        attempt_id,
        &revision,
        revision_id,
        &cancellation,
    )
    .await?;

    let CanonicalGraphStageOutcome { graph_ready, graph_degraded, pending_summary_refresh } =
        extract_canonical_graph(
            state,
            worker_id,
            job,
            attempt_id,
            document_id,
            &revision,
            revision_id,
            &cancellation,
        )
        .await?;

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

    if let Some(embedding_profile_key) = embed_chunk_profile_key.as_deref()
        && let Err(error) =
            ensure_active_embedding_profile_key(state, revision.library_id, embedding_profile_key)
                .await
    {
        let reason =
            format!("embedding execution profile changed before revision readiness: {error:#}");
        return Err(anyhow::Error::new(CanonicalContentPipelineFailure {
            delete_vectors: true,
            source: error.context(reason),
        }));
    }

    let mutation_id =
        job.mutation_id.context("canonical content ingest job is missing mutation_id")?;
    let mutation_item_id =
        job.mutation_item_id.context("canonical content ingest job is missing mutation_item_id")?;
    let now = Utc::now();
    let graph_state = if graph_degraded {
        GRAPH_STATE_DEGRADED
    } else {
        graph_state_after_successful_extract(graph_ready)
    };
    let publication = ingest_repository::publish_content_ingest_success(
        &state.persistence.postgres,
        &ingest_repository::PublishContentIngestSuccess {
            workspace_id: job.workspace_id,
            library_id: job.library_id,
            document_id,
            revision_id,
            mutation_id,
            mutation_item_id,
            attempt_id,
            expected_source_truth_version: vector_write_source_truth_version,
            embedding_profile_key: embed_chunk_profile_key,
            text_state: "text_readable".to_string(),
            graph_state: graph_state.to_string(),
            text_readable_at: Some(now),
            graph_ready_at: graph_ready.then_some(now),
            completed_at: now,
        },
    )
    .await
    .context("failed to atomically publish canonical content ingest success")?;
    let (mutation_completed, source_truth_version) = match publication {
        ingest_repository::PublishContentIngestSuccessOutcome::Applied {
            mutation_completed,
            source_truth_version,
        } => (mutation_completed, source_truth_version),
        ingest_repository::PublishContentIngestSuccessOutcome::AuthorityLost { .. } => {
            return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
        }
    };

    if let Some(pending_summary_refresh) = pending_summary_refresh
        && let Err(error) = state
            .canonical_services
            .graph
            .apply_published_summary_refresh(
                state,
                revision.library_id,
                source_truth_version,
                &pending_summary_refresh,
            )
            .await
    {
        warn!(
            %worker_id,
            job_id = %job.id,
            %attempt_id,
            %revision_id,
            source_truth_version,
            ?error,
            "content lifecycle committed but canonical summary refresh failed",
        );
    }

    invalidate_library_embedding_profile_inventory(revision.library_id);
    if let Err(error) = state
        .canonical_services
        .web_ingest
        .settle_materialized_page_for_mutation_item(state, mutation_item_id)
        .await
    {
        warn!(
            job_id = %job.id,
            %attempt_id,
            %mutation_item_id,
            ?error,
            "content publication committed but web page settlement failed; read-side reconciliation will retry",
        );
    }
    if let Err(error) = state
        .canonical_services
        .content
        .converge_document_technical_facts(state, document_id, Some(revision_id))
        .await
    {
        warn!(
            %document_id,
            %revision_id,
            ?error,
            "post-publication technical fact convergence failed",
        );
    }

    let finalizing_elapsed_ms = Some(finalizing_start.elapsed().as_millis() as i64);

    if let Err(error) = state
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
    {
        warn!(
            %attempt_id,
            %revision_id,
            ?error,
            "failed to record post-publication finalizing completion event",
        );
    }

    info!(
        %attempt_id,
        %revision_id,
        mutation_completed,
        "canonical content ingest publication committed",
    );
    Ok(CanonicalContentPipelineOutcome::LifecyclePublished)
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

    #[test]
    fn vision_billing_usage_items_expand_embedded_picture_calls() {
        let usage = serde_json::json!({
            "prompt_tokens": 30,
            "completion_tokens": 6,
            "embedded_picture_ocr_usage": [
                {"prompt_tokens": 10, "completion_tokens": 2},
                {"prompt_tokens": 20, "completion_tokens": 4}
            ]
        });

        let items = vision_billing_usage_items(&usage);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["prompt_tokens"], serde_json::json!(10));
        assert_eq!(items[1]["completion_tokens"], serde_json::json!(4));
    }

    #[test]
    fn vision_billing_usage_items_keep_single_image_usage() {
        let usage = serde_json::json!({"prompt_tokens": 10, "completion_tokens": 2});

        let items = vision_billing_usage_items(&usage);

        assert_eq!(items, vec![usage]);
    }
}
