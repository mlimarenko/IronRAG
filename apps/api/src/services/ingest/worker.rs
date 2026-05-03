mod extraction;
mod failure;
mod runtime;
mod web_jobs;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::Utc;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{sync::broadcast, task::JoinHandle, time};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{content_repository, ingest_repository},
    services::{
        content::service::{MaterializeRevisionGraphCandidatesCommand, PromoteHeadCommand},
        ingest::service::{
            FinalizeAttemptCommand, INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EMBED_CHUNK,
            INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH,
            INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_FINALIZING,
            INGEST_STAGE_PREPARE_STRUCTURE, INGEST_STAGE_WEB_DISCOVERY,
            INGEST_STAGE_WEB_MATERIALIZE_PAGE, INGEST_STAGE_WEBHOOK_DELIVERY, LeaseAttemptCommand,
            RecordStageEventCommand,
        },
    },
    shared::extraction::file_extract::{FileExtractionPlan, UploadAdmissionError},
};

use self::{
    extraction::{generate_document_summary_from_blocks, resolve_canonical_extract_content},
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
/// endpoint (`cancel_jobs_for_document`). The outer execute handler converts
/// this into a `canceled` attempt finalization and does **not** mark the job
/// as `failed` — `queue_state` is already `canceled` and must be preserved.
#[derive(Debug, Error)]
#[error("canonical ingest job {job_id} was canceled by user request")]
struct JobCanceledByRequest {
    job_id: Uuid,
}

/// Shared abort flag observed by the pipeline. The heartbeat observer sets it
/// to `true` the moment it reads `queue_state='canceled'` from the database;
/// the pipeline calls `check_cancel` between stages to react.
#[derive(Debug, Clone, Default)]
pub(super) struct JobCancellationToken {
    canceled: Arc<AtomicBool>,
}

impl JobCancellationToken {
    fn new() -> Self {
        Self { canceled: Arc::new(AtomicBool::new(false)) }
    }

    pub(super) fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Relaxed)
    }

    fn mark_canceled(&self) {
        self.canceled.store(true, Ordering::Relaxed);
    }

    /// Returns `Err(JobCanceledByRequest)` if the token has been tripped. Call
    /// this between pipeline stages so the worker stops as soon as possible
    /// after the cancel request arrives.
    pub(super) fn check(&self, job_id: Uuid) -> anyhow::Result<()> {
        if self.is_canceled() {
            Err(anyhow::Error::new(JobCanceledByRequest { job_id }))
        } else {
            Ok(())
        }
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

async fn execute_canonical_ingest_job(
    state: Arc<AppState>,
    worker_id: &str,
    job: ingest_repository::IngestJobRow,
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
    let cancellation = JobCancellationToken::new();

    // The heartbeat loop does double duty: it refreshes `heartbeat_at` so the
    // stale-lease reaper leaves this attempt alone, AND it polls the job's
    // current `queue_state` so the pipeline notices a user-issued cancel
    // between stages. One DB round-trip per tick keeps the observer latency
    // bounded by `CANONICAL_HEARTBEAT_INTERVAL` (≤15s), which is tight enough
    // for an interactive "Cancel Processing" button.
    let heartbeat_flag = Arc::clone(&heartbeat_running);
    // Dedicated tiny pool — never competes with the main working pool
    // for connections, so `touch_attempt_heartbeat` always succeeds even
    // while ingest stages hold every slot in `persistence.postgres`.
    let heartbeat_pg = state.persistence.heartbeat_postgres.clone();
    let heartbeat_cancellation = cancellation.clone();
    let heartbeat_job_id = job.id;
    let heartbeat_interval =
        Duration::from_secs(state.settings.ingestion_worker_heartbeat_interval_seconds.max(1));
    tokio::spawn(async move {
        // The cancel poll only runs until we observe the first `canceled`
        // signal: after that we know the pipeline is cooperatively unwinding,
        // and the heartbeat loop switches to pure heartbeat mode so the
        // stale-lease reaper does **not** clobber the attempt with
        // `stale_heartbeat` while the worker is draining a mid-stage LLM call
        // on its way to `JobCanceledByRequest`. Without this, long-running
        // stages like `extract_graph` would see the attempt re-written to
        // `failed + lease_expired` by the reaper before they finished
        // unwinding, losing the user's cancel intent.
        let mut observed_cancel = false;
        while heartbeat_flag.load(Ordering::Relaxed) {
            time::sleep(heartbeat_interval).await;
            if !heartbeat_flag.load(Ordering::Relaxed) {
                break;
            }
            if let Err(e) =
                ingest_repository::touch_attempt_heartbeat(&heartbeat_pg, attempt_id, None).await
            {
                tracing::warn!(?e, %attempt_id, "failed to touch attempt heartbeat");
            }
            if observed_cancel {
                continue;
            }
            match ingest_repository::get_ingest_job_by_id(&heartbeat_pg, heartbeat_job_id).await {
                Ok(Some(row)) if row.queue_state == "canceled" => {
                    info!(
                        job_id = %heartbeat_job_id,
                        %attempt_id,
                        "cancellation observed on heartbeat tick, signalling pipeline abort"
                    );
                    heartbeat_cancellation.mark_canceled();
                    observed_cancel = true;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        ?e,
                        %attempt_id,
                        "heartbeat cancel poll failed; will retry on next tick"
                    );
                }
            }
        }
    });

    // Pre-lease cancellation guard: a job may have been canceled *between*
    // `claim_next_queued_ingest_job` and the point where the heartbeat loop
    // starts observing. Fold the first observation into the same path we use
    // mid-pipeline so there is exactly one cancel handling branch.
    let current_job = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job.id)
        .await
        .context("failed to reload ingest job for cancellation check")?;
    if current_job.as_ref().is_some_and(|j| j.queue_state == "canceled") {
        cancellation.mark_canceled();
    }

    let result = if cancellation.is_canceled() {
        Err(anyhow::Error::new(JobCanceledByRequest { job_id: job.id }))
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
                    &cancellation,
                )
                .await
            }
            "web_discovery" => run_canonical_web_discovery_job(&state, &job, attempt_id).await,
            "web_materialize_page" => {
                run_canonical_web_materialize_page_job(&state, &job, attempt_id).await
            }
            "webhook_delivery" => {
                crate::services::webhook::delivery::run_webhook_delivery_job(&state, &job).await
            }
            other => Err(anyhow::anyhow!("unsupported canonical ingest job kind {other}")),
        }
    };

    drop(heartbeat_guard);

    match result {
        Ok(()) => {
            state
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
                        retryable: false,
                    },
                )
                .await
                .context("failed to finalize canonical ingest attempt as succeeded")?;
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
                            failure_code: Some("canceled_by_request".to_string()),
                            retryable: false,
                        },
                    )
                    .await
                {
                    tracing::warn!(%attempt_id, ?e, "failed to finalize user-canceled attempt as canceled");
                }
                info!(
                    %worker_id,
                    %job_id,
                    %attempt_id,
                    "canonical ingest job aborted by user cancel request",
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
    cancellation: &JobCancellationToken,
) -> anyhow::Result<()> {
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

    let revision = state
        .arango_document_store
        .get_revision(revision_id)
        .await
        .context("failed to load knowledge revision")?
        .with_context(|| format!("knowledge revision {revision_id} not found"))?;

    let extracted_content = match resolve_canonical_extract_content(state, job, &revision).await {
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

    let preparation = state
        .canonical_services
        .content
        .prepare_and_persist_revision_structure(
            state,
            revision_id,
            &extracted_content.extraction_plan,
        )
        .await
        .context("failed to prepare and persist structured revision")?;

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

    // --- Stage: embed_chunk ---------------------------------------------------
    // Inline chunk embedding. The previous "deferred non-blocking" no-op
    // left `knowledge_chunk_vector` empty, so every vector-lane retrieval
    // returned zero hits and queries fell back to lexical-only results.
    // Canonical fix: embed all chunks for the just-readable revision
    // synchronously using the library's EmbedChunk binding. Failure here
    // leaves `vector_state` unpromoted — the revision is still text-
    // readable, but graph extraction still runs so the rest of the
    // pipeline doesn't stall on embedding provider hiccups.
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

    let embed_chunk_start = Instant::now();
    let embed_chunk_outcome = state
        .canonical_services
        .search
        .embed_chunks_for_revision(state, revision.library_id, revision_id)
        .await;
    let embed_chunk_elapsed_ms = Some(embed_chunk_start.elapsed().as_millis() as i64);
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
    // Canonical wall-clock cap on the entire extract_graph stage. Guards
    // against stages that silently monopolize the tokio runtime and
    // starve heartbeat/cancel polling. On timeout we fall through to the
    // same "degraded to readable" path the downstream match uses for
    // other graph extraction failures, so the attempt still finalizes
    // instead of leaking a `leased` row until the reaper catches it.
    let stage_timeout =
        Duration::from_secs(state.settings.runtime_graph_extract_stage_timeout_seconds.max(1));

    let graph_materialization = match time::timeout(
        stage_timeout,
        state.canonical_services.content.materialize_revision_graph_candidates(
            state,
            MaterializeRevisionGraphCandidatesCommand {
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                revision_id,
                attempt_id: Some(attempt_id),
            },
        ),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(crate::interfaces::http::router_support::ApiError::Conflict(format!(
            "stage_timeout: extract_graph stage exceeded canonical timeout of {}s during graph candidate materialization",
            stage_timeout.as_secs()
        ))),
    };
    let mut graph_ready = false;

    match graph_materialization {
        Ok(graph_materialization) => {
            let graph_outcome = match time::timeout(
                stage_timeout,
                state.canonical_services.graph.reconcile_revision_graph(
                    state,
                    job.library_id,
                    document_id,
                    revision_id,
                    Some(attempt_id),
                ),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "extract_graph stage exceeded canonical timeout of {}s during revision graph reconcile",
                    stage_timeout.as_secs()
                )),
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
                                message: Some("graph candidates extracted and reconciled".to_string()),
                                details_json: serde_json::json!({
                                    "chunksProcessed": graph_materialization.chunk_count,
                                    "graphChunksSelected": graph_materialization.selected_graph_chunks,
                                    "recordStreamSourceUnitsSkipped": graph_materialization.record_stream_source_units_skipped,
                                    "extractedEntityCandidates": graph_materialization.extracted_entities,
                                    "extractedRelationCandidates": graph_materialization.extracted_relations,
                                    "reusedChunks": graph_materialization.reused_chunks,
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

                    // Capture graph-embedding billing under its own
                    // `embed_graph` call_kind. Previous versions filed
                    // this usage under the `embed_chunk` stage event,
                    // which made every dashboard conflate chunk-embed
                    // activity (which was actually a no-op until this
                    // release) with graph-node/edge embedding activity.
                    if let Some(embedding_usage) = outcome.embedding_usage {
                        let embed_provider = embedding_usage.provider_kind.clone();
                        let embed_model = embedding_usage.model_name.clone();

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
                                    provider_kind: embed_provider.clone().unwrap_or_default(),
                                    model_name: embed_model.clone().unwrap_or_default(),
                                    call_kind: "embed_graph".to_string(),
                                    usage_json: embedding_usage.into_usage_json(),
                                },
                            )
                            .await
                        {
                            warn!(%worker_id, job_id = %job.id, ?e, "embed_graph billing capture failed");
                        }
                    }
                }
                Err(graph_error) => {
                    warn!(
                        %worker_id,
                        job_id = %job.id,
                        revision_id = %revision_id,
                        ?graph_error,
                        "canonical graph rebuild failed; preserving readable revision",
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
                                    "graph rebuild failed; readable revision preserved".to_string(),
                                ),
                                details_json: serde_json::json!({
                                    "chunksProcessed": graph_materialization.chunk_count,
                                    "graphChunksSelected": graph_materialization.selected_graph_chunks,
                                    "recordStreamSourceUnitsSkipped": graph_materialization.record_stream_source_units_skipped,
                                    "extractedEntityCandidates": graph_materialization.extracted_entities,
                                    "extractedRelationCandidates": graph_materialization.extracted_relations,
                                    "graphReady": false,
                                    "degradedToReadable": true,
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
            warn!(
                %worker_id,
                job_id = %job.id,
                revision_id = %revision_id,
                ?error,
                "graph candidate extraction failed; preserving readable revision",
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
                        message: Some(
                            "graph candidate extraction failed; readable revision preserved"
                                .to_string(),
                        ),
                        details_json: serde_json::json!({
                            "graphReady": false,
                            "degradedToReadable": true,
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

    // --- Graph maintenance (entity resolution + community detection) ---
    //
    // Entity resolution walks O(nodes) comparing pairs, community
    // detection runs label-propagation over O(nodes + edges), and
    // `generate_community_summaries` does one LLM call per community.
    // On a mid-sized library one pass is a few CPU-seconds plus a
    // handful of LLM round-trips.
    //
    // Without the throttle below, these three ran at the end of every
    // single ingest job — under a burst of parallel workers each
    // finishing job kicked another full-library pass while the previous
    // one was still running, and the maintenance loop became the
    // dominant CPU sink instead of the actual extract/merge work.
    //
    // The work is idempotent and library-wide, so compressing a burst
    // of finalising jobs into one maintenance pass per library per
    // interval is safe. `try_acquire_graph_maintenance_slot` gives us
    // exactly that: the first job to finish in a window claims the
    // pass, the rest skip the block entirely.
    if graph_ready
        && crate::services::graph::maintenance::try_acquire_graph_maintenance_slot(job.library_id)
    {
        if let Err(error) = crate::services::graph::entity_resolution::resolve_after_ingestion(
            state,
            job.library_id,
        )
        .await
        {
            tracing::warn!(library_id = %job.library_id, ?error, "entity resolution failed, continuing");
        }

        if let Err(error) = crate::services::graph::community_detection::detect_after_ingestion(
            state,
            job.library_id,
        )
        .await
        {
            tracing::warn!(library_id = %job.library_id, ?error, "community detection failed, continuing");
        }

        // Generate community summaries from top entities and relationships
        if let Err(error) =
            crate::services::graph::community_detection::generate_community_summaries(
                state,
                job.library_id,
            )
            .await
        {
            tracing::warn!(library_id = %job.library_id, ?error, "community summary generation failed, continuing");
        }
    } else if graph_ready {
        tracing::debug!(
            library_id = %job.library_id,
            "graph maintenance skipped — another ingest job already ran the pass in this window"
        );
    }

    // --- Graph backfill (self-healing pass for failed extract_graph stages) ---
    //
    // When `extract_graph` fails at the stage level (canonical 600s timeout,
    // projection write failure, cancellation) but individual chunk
    // extractions already persisted `ready` rows in `runtime_graph_extraction`,
    // this job's `reconcile_revision_graph` never runs — the document's
    // entities sit ready in Postgres but never become graph nodes. The
    // dashboard then shows the doc as "readable" while the graph viewer
    // never learns it exists.
    //
    // Run regardless of the current job's `graph_ready` flag — the backfill
    // target is the set of ALL library documents that got stuck, not
    // whatever the current job produced. A dedicated 60s slot stops a
    // queue burst from replaying the same pass in a tight loop.
    if crate::services::graph::backfill::try_acquire_graph_backfill_slot(job.library_id) {
        if let Err(error) =
            crate::services::graph::backfill::run_library_graph_backfill(state, job.library_id)
                .await
        {
            tracing::warn!(
                library_id = %job.library_id,
                ?error,
                "graph backfill pass failed, continuing"
            );
        }
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
    cancellation.check(job.id)?;
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
            if graph_ready { "ready" } else { "processing" },
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
