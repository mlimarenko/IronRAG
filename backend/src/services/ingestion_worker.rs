use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tokio::{sync::broadcast, task::JoinHandle, time};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::config::Settings,
    app::state::AppState,
    domains::{
        billing::{PricingBillingUnit, PricingCapability, PricingResolutionStatus},
        runtime_graph::RuntimeNodeType,
    },
    infra::{
        arangodb::document_store::{KnowledgeLibraryGenerationRow, KnowledgeRevisionRow},
        repositories::{self, IngestionJobRow, content_repository, ingest_repository},
    },
    services::{
        document_accounting,
        extract_service::PersistExtractContentCommand,
        graph_extract::{
            GraphExtractionCandidateSet, GraphExtractionRecoveryRecord, GraphExtractionRequest,
            GraphExtractionResumeHint, GraphExtractionTelemetrySummary,
            extract_and_persist_chunk_graph_result, extraction_outcome_from_resume_state,
            summarize_graph_extraction_usage_calls,
        },
        graph_merge::{
            GraphMergeScope, merge_chunk_graph_candidates, reconcile_merge_support_counts,
        },
        graph_projection::{project_canonical_graph, resolve_projection_scope},
        graph_reconciliation_scope::persist_detected_scope,
        graph_summary::GraphSummaryRefreshRequest,
        ingest_service::{FinalizeAttemptCommand, LeaseAttemptCommand, RecordStageEventCommand},
        knowledge_service::{
            CreateKnowledgeChunkCommand, CreateKnowledgeDocumentCommand,
            CreateKnowledgeRevisionCommand, PromoteKnowledgeDocumentCommand,
            RefreshKnowledgeLibraryGenerationCommand,
        },
        query_support::invalidate_library_source_truth,
        runtime_ingestion::{
            JobLeaseHeartbeat, RuntimeStageUsageSummary, embed_runtime_chunks,
            embed_runtime_graph_edges, embed_runtime_graph_nodes,
            persist_extracted_content_from_payload, resolve_effective_provider_profile,
            resolve_runtime_run_provider_profile,
            upsert_runtime_document_chunk_contribution_summary,
            upsert_runtime_document_graph_contribution_summary,
        },
    },
    shared::{
        chunking::{ChunkingProfile, split_text_into_chunks_with_profile},
        file_extract::UploadAdmissionError,
    },
};

const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_WORKER_LEASE_DURATION: Duration = Duration::from_secs(300);
const DEFAULT_WORKER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_STALE_WORKER_GRACE_SECONDS: i64 = 45;
const CANONICAL_LEASE_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);
const CANONICAL_STALE_LEASE_SECONDS: i64 = 120;
const CANONICAL_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const EXTRACTING_GRAPH_PROGRESS_START_PERCENT: i32 = 82;
const EXTRACTING_GRAPH_PROGRESS_END_PERCENT: i32 = 87;
const MERGING_GRAPH_PROGRESS_START_PERCENT: i32 = 88;
#[cfg(test)]
const GRAPH_PROGRESS_ACTIVITY_INTERVAL: Duration = Duration::from_secs(30);
const RUNTIME_STAGE_SEQUENCE: [&str; 7] = [
    "extracting_content",
    "chunking",
    "embedding_chunks",
    "extracting_graph",
    "merging_graph",
    "projecting_graph",
    "finalizing",
];

#[derive(Debug, Clone)]
struct WorkerDocumentContext {
    document: repositories::DocumentRow,
    document_for_processing: repositories::DocumentRow,
    target_revision_id: Option<Uuid>,
    target_revision: Option<repositories::DocumentRevisionRow>,
    previous_active_revision: Option<repositories::DocumentRevisionRow>,
    old_chunk_ids: Vec<Uuid>,
}

#[derive(Debug, Clone)]
struct RuntimeStageSpan {
    stage_event_id: Uuid,
    stage: String,
    started_at: DateTime<Utc>,
    provider_kind: Option<String>,
    model_name: Option<String>,
}

#[derive(Debug, Clone)]
struct GraphStageProgressTracker {
    last_persisted_progress: i32,
    last_persisted_at: Instant,
    processed_chunks: usize,
    provider_call_count: usize,
    total_call_elapsed_ms: i64,
    chars_per_second_sum: f64,
    chars_per_second_samples: usize,
    tokens_per_second_sum: f64,
    tokens_per_second_samples: usize,
    last_provider_call_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct CanonicalExtractContentError {
    failure_code: String,
    retryable: bool,
    message: String,
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

struct CanonicalExtractedContent {
    text: String,
    warning_count: i32,
    stage_details: serde_json::Value,
}

impl GraphStageProgressTracker {
    fn record_extraction(&mut self, telemetry: &GraphExtractionTelemetrySummary) {
        self.processed_chunks += 1;
        self.provider_call_count += telemetry.provider_call_count;
        self.total_call_elapsed_ms =
            self.total_call_elapsed_ms.saturating_add(telemetry.total_call_elapsed_ms.max(0));
        if let Some(value) = telemetry.avg_chars_per_second {
            self.chars_per_second_sum += value;
            self.chars_per_second_samples += 1;
        }
        if let Some(value) = telemetry.avg_tokens_per_second {
            self.tokens_per_second_sum += value;
            self.tokens_per_second_samples += 1;
        }
        if let Some(finished_at) = telemetry.last_provider_call_at {
            self.last_provider_call_at = Some(
                self.last_provider_call_at.map_or(finished_at, |current| current.max(finished_at)),
            );
        }
    }

    fn record_resumed_chunk(&mut self) {
        self.processed_chunks += 1;
    }

    fn avg_call_elapsed_ms(&self) -> Option<i64> {
        (self.provider_call_count > 0).then(|| {
            self.total_call_elapsed_ms / i64::try_from(self.provider_call_count).unwrap_or(1)
        })
    }

    fn avg_chunk_elapsed_ms(&self) -> Option<i64> {
        (self.processed_chunks > 0)
            .then(|| self.total_call_elapsed_ms / i64::try_from(self.processed_chunks).unwrap_or(1))
    }

    fn avg_chars_per_second(&self) -> Option<f64> {
        (self.chars_per_second_samples > 0)
            .then(|| self.chars_per_second_sum / self.chars_per_second_samples as f64)
    }

    fn avg_tokens_per_second(&self) -> Option<f64> {
        (self.tokens_per_second_samples > 0)
            .then(|| self.tokens_per_second_sum / self.tokens_per_second_samples as f64)
    }

    fn next_checkpoint_eta_ms(&self, total_chunks: usize) -> Option<i64> {
        let remaining_chunks = total_chunks.saturating_sub(self.processed_chunks);
        match (remaining_chunks, self.avg_chunk_elapsed_ms()) {
            (0, _) => Some(0),
            (_, Some(avg_chunk_elapsed_ms)) if avg_chunk_elapsed_ms > 0 => Some(
                avg_chunk_elapsed_ms
                    .saturating_mul(i64::try_from(remaining_chunks).unwrap_or(i64::MAX)),
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreservedFailureRevisionTruth {
    text_state: String,
    vector_state: String,
    graph_state: String,
    text_readable_at: Option<DateTime<Utc>>,
    vector_ready_at: Option<DateTime<Utc>>,
    graph_ready_at: Option<DateTime<Utc>>,
}

impl PreservedFailureRevisionTruth {
    fn text_ready(&self) -> bool {
        self.text_readable_at.is_some() || text_stage_is_ready(&self.text_state)
    }

    fn vector_ready(&self) -> bool {
        self.vector_ready_at.is_some() || vector_stage_is_ready(&self.vector_state)
    }

    fn graph_ready(&self) -> bool {
        self.graph_ready_at.is_some() || graph_stage_is_ready(&self.graph_state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FailureGenerationSnapshot {
    generation_id: Uuid,
    active_text_generation: i64,
    active_vector_generation: i64,
    active_graph_generation: i64,
    degraded_state: String,
}

fn graph_extraction_downgrade_level(state: &AppState, replay_count: usize) -> usize {
    let level_one =
        state.resolve_settle_blockers.extraction_resume_downgrade_level_one_after_replays;
    let level_two = state
        .resolve_settle_blockers
        .extraction_resume_downgrade_level_two_after_replays
        .max(level_one.saturating_add(1));
    if replay_count >= level_two {
        2
    } else if replay_count >= level_one {
        1
    } else {
        0
    }
}

pub fn spawn_ingestion_worker(
    state: AppState,
    shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_ingestion_worker_pool(Arc::new(state), shutdown).await;
    })
}

async fn run_ingestion_worker_pool(state: Arc<AppState>, mut shutdown: broadcast::Receiver<()>) {
    let legacy_runtime_tables_present =
        match crate::infra::persistence::legacy_runtime_repair_tables_present(
            &state.persistence.postgres,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                warn!(?error, "failed to inspect legacy runtime ingestion tables");
                false
            }
        };

    let worker_concurrency = state.settings.ingestion_worker_concurrency.max(1);

    let mut handles = Vec::new();

    if legacy_runtime_tables_present {
        info!(worker_concurrency, "starting ingestion worker pool with legacy + canonical queues");
        handles.push(tokio::spawn(run_lease_recovery_loop(
            state.clone(),
            shutdown.resubscribe(),
            lease_recovery_worker_id(&state.settings.service_name),
        )));
        for worker_index in 0..worker_concurrency {
            let worker_id = ingestion_worker_id(&state.settings.service_name, worker_index);
            handles.push(tokio::spawn(run_ingestion_worker_loop(
                state.clone(),
                shutdown.resubscribe(),
                worker_id,
            )));
        }
    } else {
        info!("legacy runtime ingestion queue tables absent; running canonical queue only");
    }

    handles.push(tokio::spawn(run_canonical_lease_recovery_loop(
        state.clone(),
        shutdown.resubscribe(),
    )));

    for worker_index in 0..worker_concurrency {
        let worker_id = canonical_worker_id(&state.settings.service_name, worker_index);
        handles.push(tokio::spawn(run_canonical_ingest_worker_loop(
            state.clone(),
            shutdown.resubscribe(),
            worker_id,
        )));
    }

    if handles.is_empty() {
        let _ = shutdown.recv().await;
        return;
    }

    for handle in handles {
        if let Err(error) = handle.await {
            error!(?error, "ingestion worker task crashed");
        }
    }
}

fn ingestion_worker_id(service_name: &str, worker_index: usize) -> String {
    format!("{service_name}:{worker_index}:{}", Uuid::now_v7())
}

fn lease_recovery_worker_id(service_name: &str) -> String {
    format!("{service_name}:lease-recovery")
}

fn canonical_worker_id(service_name: &str, worker_index: usize) -> String {
    format!("{service_name}:canonical:{worker_index}:{}", Uuid::now_v7())
}

async fn run_canonical_ingest_worker_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
    worker_id: String,
) {
    info!(%worker_id, "starting canonical ingest worker loop");

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!(%worker_id, "stopping canonical ingest worker loop");
                break;
            }
            _ = time::sleep(WORKER_POLL_INTERVAL) => {
                match ingest_repository::claim_next_queued_ingest_job(
                    &state.persistence.postgres,
                ).await {
                    Ok(Some(job)) => {
                        let job_id = job.id;
                        let started_at = Instant::now();
                        info!(
                            %worker_id,
                            %job_id,
                            job_kind = %job.job_kind,
                            library_id = %job.library_id,
                            "claimed canonical ingest job",
                        );
                        if let Err(error) = execute_canonical_ingest_job(
                            state.clone(), &worker_id, job,
                        ).await {
                            let elapsed_ms = started_at.elapsed().as_millis();
                            error!(
                                %worker_id,
                                %job_id,
                                elapsed_ms,
                                ?error,
                                "canonical ingest job failed",
                            );
                            fail_canonical_ingest_job(
                                &state, job_id, &worker_id, &error,
                            ).await;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(%worker_id, ?error, "failed to claim canonical ingest job");
                    }
                }
            }
        }
    }
}

async fn run_canonical_lease_recovery_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    info!("starting canonical lease recovery loop");
    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("stopping canonical lease recovery loop");
                break;
            }
            _ = time::sleep(CANONICAL_LEASE_RECOVERY_INTERVAL) => {
                let threshold = chrono::Duration::seconds(CANONICAL_STALE_LEASE_SECONDS);
                match ingest_repository::recover_stale_canonical_leases(
                    &state.persistence.postgres,
                    threshold,
                ).await {
                    Ok(0) => {}
                    Ok(recovered) => {
                        warn!(recovered, "recovered stale canonical ingest job leases");
                    }
                    Err(error) => {
                        warn!(?error, "failed to recover stale canonical leases");
                    }
                }
            }
        }
    }
}

async fn run_lease_recovery_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
    worker_id: String,
) {
    info!(%worker_id, "starting ingestion lease recovery loop");

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!(%worker_id, "stopping ingestion lease recovery loop");
                break;
            }
            _ = time::sleep(WORKER_POLL_INTERVAL) => {
                if let Err(error) = recover_expired_leases(state.as_ref(), &worker_id).await {
                    warn!(%worker_id, ?error, "failed to recover expired ingestion job leases");
                }
            }
        }
    }
}

async fn run_ingestion_worker_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
    worker_id: String,
) {
    info!(%worker_id, "starting ingestion worker loop");

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!(%worker_id, "stopping ingestion worker loop");
                break;
            }
            _ = time::sleep(WORKER_POLL_INTERVAL) => {
                match repositories::claim_next_ingestion_job(
                    &state.persistence.postgres,
                    &worker_id,
                    worker_lease_duration(&state.settings),
                    state.pipeline_hardening.total_worker_slots,
                    state.pipeline_hardening.minimum_slice_capacity,
                ).await {
                    Ok(Some(job)) => {
                        let job_id = job.id;
                        let attempt_no = job.attempt_count;
                        let runtime_ingestion_run_id = repositories::parse_ingestion_execution_payload(&job)
                            .ok()
                            .and_then(|payload| payload.runtime_ingestion_run_id);
                        let started_at = Instant::now();
                        info!(
                            %worker_id,
                            job_id = %job_id,
                            project_id = %job.project_id,
                            source_id = ?job.source_id,
                            attempt_no,
                            trigger_kind = %job.trigger_kind,
                            "claimed ingestion job",
                        );
                        if let Err(error) = execute_job(state.clone(), &worker_id, job).await {
                            error!(
                                %worker_id,
                                job_id = %job_id,
                                attempt_no,
                                elapsed_ms = started_at.elapsed().as_millis(),
                                ?error,
                                "ingestion worker job execution crashed",
                            );
                            fail_job(
                                &state,
                                job_id,
                                Some(attempt_no),
                                runtime_ingestion_run_id,
                                &worker_id,
                                started_at.elapsed().as_millis(),
                                &error,
                            )
                            .await;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(%worker_id, ?error, "failed to claim ingestion job");
                    }
                }
            }
        }
    }
}

async fn execute_job(
    state: Arc<AppState>,
    worker_id: &str,
    job: IngestionJobRow,
) -> anyhow::Result<()> {
    let attempt_no = job.attempt_count;
    let started_at = Instant::now();
    let payload = repositories::parse_ingestion_execution_payload(&job)
        .context("ingestion job payload missing or invalid")?;
    let runtime_ingestion_run_id = payload.runtime_ingestion_run_id;
    let workspace_id =
        repositories::get_project_by_id(&state.persistence.postgres, payload.project_id)
            .await
            .context("failed to load project while preparing stage accounting")?
            .map(|project| project.workspace_id);
    let runtime_run = if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        repositories::get_runtime_ingestion_run_by_id(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
        )
        .await
        .context("failed to load runtime ingestion run for worker execution")?
    } else {
        None
    };
    let provider_profile = if let Some(runtime_run) = runtime_run.as_ref() {
        resolve_runtime_run_provider_profile(runtime_run)?
    } else {
        resolve_effective_provider_profile(state.as_ref(), payload.project_id).await?
    };
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        repositories::mark_runtime_ingestion_run_claimed(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
            Utc::now(),
        )
        .await
        .context("failed to mark runtime ingestion run as claimed")?;
    }
    let mutation_document = if let Some(document_id) =
        payload.logical_document_id.or(runtime_run.as_ref().and_then(|row| row.document_id))
    {
        Some(
            repositories::get_document_by_id(&state.persistence.postgres, document_id)
                .await
                .with_context(|| format!("failed to load logical document {document_id}"))?
                .with_context(|| format!("logical document {document_id} not found"))?,
        )
    } else {
        None
    };
    let previous_active_revision = if let Some(document) = &mutation_document {
        match document.current_revision_id {
            Some(revision_id) => {
                repositories::get_document_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .with_context(|| {
                        format!("failed to load active document revision {revision_id}")
                    })?
            }
            None => None,
        }
    } else {
        None
    };
    if let Some(document) = &mutation_document {
        if document.deleted_at.is_some() {
            anyhow::bail!("stale revision attempt rejected: logical document has been deleted");
        }
    }
    if let Some(stale_guard_revision_no) = payload.stale_guard_revision_no {
        let active_revision_no = previous_active_revision.as_ref().map(|row| row.revision_no);
        if active_revision_no != Some(stale_guard_revision_no) {
            anyhow::bail!(
                "stale revision attempt rejected: expected active revision {}, found {:?}",
                stale_guard_revision_no,
                active_revision_no
            );
        }
    }
    if let Some(mutation_workflow_id) = payload.document_mutation_workflow_id {
        repositories::update_document_mutation_workflow_status(
            &state.persistence.postgres,
            mutation_workflow_id,
            "reconciling",
            None,
        )
        .await
        .with_context(|| {
            format!(
                "failed to mark document mutation workflow {mutation_workflow_id} as reconciling"
            )
        })?;
    }
    if let Some(document) = &mutation_document {
        repositories::update_document_current_revision(
            &state.persistence.postgres,
            document.id,
            document.current_revision_id,
            "reconciling",
            payload.mutation_kind.as_deref(),
            payload.mutation_kind.as_deref().map(|_| "reconciling"),
        )
        .await
        .with_context(|| {
            format!("failed to mark logical document {} as reconciling", document.id)
        })?;
    }
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, payload.project_id)
            .await
            .context("failed to load graph snapshot before worker execution")?;
    let rebuild_follow_up =
        is_rebuild_follow_up_job(&job, snapshot.as_ref().map(|row| row.graph_status.as_str()));
    let text = payload.text.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            payload.extraction_error.clone().unwrap_or_else(|| {
                "no extracted text payload is available for this ingestion job".to_string()
            })
        )
    })?;

    info!(
        job_id = %job.id,
        %worker_id,
        project_id = %payload.project_id,
        source_id = ?payload.source_id,
        attempt_no,
        external_key = %payload.external_key,
        ingest_mode = %payload.ingest_mode,
        text_len = text.len(),
        "starting ingestion job",
    );
    let mut lease_heartbeat = JobLeaseHeartbeat::new(
        job.id,
        worker_id,
        runtime_ingestion_run_id,
        worker_lease_duration(&state.settings),
        worker_heartbeat_interval(&state.settings),
    );
    let _lease_keep_alive = lease_heartbeat.spawn_keep_alive(state.clone());

    let extracting_content_span = start_runtime_stage(
        state.as_ref(),
        runtime_ingestion_run_id,
        attempt_no,
        "extracting_content",
        Some(20),
        Some(extracting_content_stage_message(rebuild_follow_up)),
        job.id,
        payload.extraction_provider_kind.as_deref(),
        payload.extraction_model_name.as_deref(),
    )
    .await?;
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        persist_extracted_content_from_payload(
            state.as_ref(),
            runtime_ingestion_run_id,
            None,
            &payload,
        )
        .await?;
        let extracting_content_event = complete_runtime_stage(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            extracting_content_span
                .as_ref()
                .expect("runtime stage span must exist when runtime run id exists"),
            Some("extracted content is ready for chunking"),
            job.id,
        )
        .await?;
        maybe_record_extraction_stage_accounting(
            state.as_ref(),
            workspace_id,
            payload.project_id,
            runtime_ingestion_run_id,
            "extracting_content",
            &extracting_content_event,
            payload.extraction_provider_kind.as_deref(),
            payload.extraction_model_name.as_deref(),
        )
        .await?;
    }

    info!(
        job_id = %job.id,
        %worker_id,
        project_id = %payload.project_id,
        source_id = ?payload.source_id,
        attempt_no,
        stage = "persisting_document",
        "ingestion job stage started",
    );
    repositories::mark_ingestion_job_stage(
        &state.persistence.postgres,
        job.id,
        worker_id,
        "running",
        "persisting_document",
        None,
    )
    .await?;
    repositories::mark_ingestion_job_attempt_stage(
        &state.persistence.postgres,
        job.id,
        attempt_no,
        worker_id,
        "running",
        "persisting_document",
        None,
    )
    .await?;

    let checksum = sha256_hex(text);
    let document_context = ensure_worker_document(
        state.as_ref(),
        workspace_id,
        &payload,
        runtime_ingestion_run_id,
        mutation_document,
        previous_active_revision,
        &checksum,
        text.len(),
    )
    .await?;
    let document = document_context.document.clone();
    let document_for_processing = document_context.document_for_processing.clone();
    let target_revision = document_context.target_revision.as_ref().with_context(|| {
        format!(
            "ingestion job {} missing target revision context after document preparation",
            job.id
        )
    })?;
    let revision_generation = i64::from(target_revision.revision_no);

    info!(
        job_id = %job.id,
        %worker_id,
        project_id = %payload.project_id,
        document_id = %document.id,
        checksum = %checksum,
        "persisted ingestion document",
    );

    let chunking_span = start_runtime_stage(
        state.as_ref(),
        runtime_ingestion_run_id,
        attempt_no,
        "chunking",
        Some(65),
        Some(chunking_stage_message(rebuild_follow_up)),
        job.id,
        None,
        None,
    )
    .await?;
    info!(
        job_id = %job.id,
        %worker_id,
        project_id = %payload.project_id,
        document_id = %document.id,
        attempt_no,
        stage = "chunking",
        "ingestion job stage started",
    );
    repositories::mark_ingestion_job_stage(
        &state.persistence.postgres,
        job.id,
        worker_id,
        "running",
        "chunking",
        None,
    )
    .await?;
    repositories::mark_ingestion_job_attempt_stage(
        &state.persistence.postgres,
        job.id,
        attempt_no,
        worker_id,
        "running",
        "chunking",
        None,
    )
    .await?;

    let chunks = split_text_into_chunks_with_profile(text, ChunkingProfile::default());
    if chunks.is_empty() {
        warn!(
            job_id = %job.id,
            %worker_id,
            project_id = %payload.project_id,
            document_id = %document.id,
            text_len = text.len(),
            "ingestion job produced zero chunks",
        );
    } else {
        info!(
            job_id = %job.id,
            %worker_id,
            project_id = %payload.project_id,
            document_id = %document.id,
            chunk_count = chunks.len(),
            "prepared ingestion chunks",
        );
    }
    let mut chunk_count = 0usize;
    let mut persisted_chunks = Vec::with_capacity(chunks.len());
    for (idx, content) in chunks.iter().enumerate() {
        if idx % 16 == 0 {
            lease_heartbeat.maybe_renew(state.as_ref()).await?;
        }
        let chunk = repositories::create_chunk(
            &state.persistence.postgres,
            document.id,
            payload.project_id,
            i32::try_from(idx).unwrap_or(i32::MAX),
            content,
            Some(i32::try_from(content.split_whitespace().count()).unwrap_or(i32::MAX)),
            serde_json::json!({
                "ingest_mode": payload.ingest_mode,
                "extra": payload.extra_metadata,
                "ingestion_job_id": job.id,
                "runtime_ingestion_run_id": payload.runtime_ingestion_run_id,
                "extraction_kind": payload.extraction_kind,
                "page_count": payload.page_count,
                "source_map": payload.source_map,
            }),
        )
        .await?;
        persist_worker_chunk_knowledge(
            state.as_ref(),
            workspace_id,
            payload.project_id,
            target_revision,
            &chunk,
            "chunked",
            None,
        )
        .await?;
        persisted_chunks.push(chunk);
        chunk_count += 1;
    }
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        complete_runtime_stage(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            chunking_span
                .as_ref()
                .expect("runtime stage span must exist when runtime run id exists"),
            Some(chunking_completed_message(rebuild_follow_up)),
            job.id,
        )
        .await?;
        upsert_runtime_document_chunk_contribution_summary(
            state.as_ref(),
            document.id,
            document_context.target_revision_id.or(document.current_revision_id),
            runtime_ingestion_run_id,
            attempt_no,
            chunk_count,
        )
        .await?;
    }

    repositories::mark_ingestion_job_stage(
        &state.persistence.postgres,
        job.id,
        worker_id,
        "running",
        "embedding_chunks",
        None,
    )
    .await?;
    repositories::mark_ingestion_job_attempt_stage(
        &state.persistence.postgres,
        job.id,
        attempt_no,
        worker_id,
        "running",
        "embedding_chunks",
        None,
    )
    .await?;
    let embedding_chunks_span = start_runtime_stage(
        state.as_ref(),
        runtime_ingestion_run_id,
        attempt_no,
        "embedding_chunks",
        Some(74),
        Some(embedding_chunks_stage_message(rebuild_follow_up)),
        job.id,
        Some(provider_profile.embedding.provider_kind.as_str()),
        Some(&provider_profile.embedding.model_name),
    )
    .await?;
    let embedding_chunks_usage = embed_runtime_chunks(
        state.as_ref(),
        &provider_profile,
        &persisted_chunks,
        Some(&mut lease_heartbeat),
    )
    .await?;
    let vectorized_chunk_count = persist_worker_chunk_vectors(
        state.as_ref(),
        workspace_id,
        payload.project_id,
        target_revision,
        revision_generation,
        &persisted_chunks,
    )
    .await?;
    if vectorized_chunk_count != persisted_chunks.len() {
        warn!(
            job_id = %job.id,
            %worker_id,
            document_id = %document.id,
            expected_chunk_vectors = persisted_chunks.len(),
            written_chunk_vectors = vectorized_chunk_count,
            "not all chunk vectors could be mirrored into ArangoDB"
        );
    }
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        let embedding_chunks_event = complete_runtime_stage(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            embedding_chunks_span
                .as_ref()
                .expect("runtime stage span must exist when runtime run id exists"),
            Some(embedding_chunks_completed_message(rebuild_follow_up)),
            job.id,
        )
        .await?;
        maybe_record_usage_stage_accounting(
            state.as_ref(),
            workspace_id,
            payload.project_id,
            runtime_ingestion_run_id,
            "embedding_chunks",
            &embedding_chunks_event,
            PricingCapability::Embedding,
            PricingBillingUnit::Per1MInputTokens,
            "runtime_document_embedding_chunks",
            None,
            &embedding_chunks_usage,
        )
        .await?;
    }

    repositories::mark_ingestion_job_stage(
        &state.persistence.postgres,
        job.id,
        worker_id,
        "running",
        "extracting_graph",
        None,
    )
    .await?;
    repositories::mark_ingestion_job_attempt_stage(
        &state.persistence.postgres,
        job.id,
        attempt_no,
        worker_id,
        "running",
        "extracting_graph",
        None,
    )
    .await?;
    let extracting_graph_span = start_runtime_stage(
        state.as_ref(),
        runtime_ingestion_run_id,
        attempt_no,
        "extracting_graph",
        Some(EXTRACTING_GRAPH_PROGRESS_START_PERCENT),
        Some(extracting_graph_stage_message(rebuild_follow_up)),
        job.id,
        Some(provider_profile.indexing.provider_kind.as_str()),
        Some(&provider_profile.indexing.model_name),
    )
    .await?;
    let projection_scope = resolve_projection_scope(state.as_ref(), payload.project_id).await?;
    let mut chunk_graph_results = Vec::new();
    let mut graph_extract_usage = RuntimeStageUsageSummary::with_model(
        provider_profile.indexing.provider_kind.as_str(),
        &provider_profile.indexing.model_name,
    );
    let mut graph_extract_call_sequence_no = 0_i32;
    let mut graph_progress_tracker = GraphStageProgressTracker {
        last_persisted_progress: EXTRACTING_GRAPH_PROGRESS_START_PERCENT,
        last_persisted_at: Instant::now(),
        processed_chunks: 0,
        provider_call_count: 0,
        total_call_elapsed_ms: 0,
        chars_per_second_sum: 0.0,
        chars_per_second_samples: 0,
        tokens_per_second_sum: 0.0,
        tokens_per_second_samples: 0,
        last_provider_call_at: None,
    };
    let mut graph_resume_rows_by_ordinal = BTreeMap::new();
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        for row in repositories::list_runtime_graph_extraction_resume_states_by_run(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
        )
        .await?
        {
            graph_resume_rows_by_ordinal.insert(row.chunk_ordinal, row);
        }
    }
    for (chunk_index, chunk) in persisted_chunks.iter().enumerate() {
        lease_heartbeat.maybe_renew(state.as_ref()).await?;
        let chunk_content_hash = sha256_hex(&chunk.content);
        if let (Some(runtime_ingestion_run_id), Some(existing_resume_row)) =
            (runtime_ingestion_run_id, graph_resume_rows_by_ordinal.get(&chunk.ordinal))
        {
            if existing_resume_row.status == "ready"
                && existing_resume_row.chunk_content_hash == chunk_content_hash
            {
                let resumed_row = repositories::increment_runtime_graph_extraction_resume_hit(
                    &state.persistence.postgres,
                    runtime_ingestion_run_id,
                    chunk.ordinal,
                )
                .await?;
                let extracted = extraction_outcome_from_resume_state(&resumed_row)
                    .context("failed to rebuild graph extraction outcome from resume state")?;
                graph_progress_tracker.record_resumed_chunk();
                if !extracted.normalized.entities.is_empty()
                    || !extracted.normalized.relations.is_empty()
                {
                    chunk_graph_results.push((
                        chunk.clone(),
                        extracted.normalized,
                        extracted.recovery_summary,
                    ));
                }
                maybe_persist_graph_progress_checkpoint(
                    state.as_ref(),
                    Some(runtime_ingestion_run_id),
                    attempt_no,
                    &mut graph_progress_tracker,
                    chunk_index + 1,
                    persisted_chunks.len(),
                )
                .await?;
                continue;
            }
        }
        let resume_hint = graph_resume_rows_by_ordinal
            .get(&chunk.ordinal)
            .filter(|row| row.chunk_content_hash == chunk_content_hash)
            .map(|row| GraphExtractionResumeHint {
                replay_count: usize::try_from(row.replay_count.max(0)).unwrap_or(usize::MAX),
                downgrade_level: graph_extraction_downgrade_level(
                    state.as_ref(),
                    usize::try_from(row.replay_count.max(0)).unwrap_or(usize::MAX),
                ),
            });
        let extraction_request = GraphExtractionRequest {
            project_id: payload.project_id,
            document: document_for_processing.clone(),
            chunk: chunk.clone(),
            revision_id: document_context.target_revision_id.or(document.current_revision_id),
            activated_by_attempt_id: runtime_ingestion_run_id,
            resume_hint,
        };
        let extracted = match extract_and_persist_chunk_graph_result(
            state.as_ref(),
            &provider_profile,
            &extraction_request,
        )
        .await
        {
            Ok(outcome) => {
                persist_graph_extraction_recovery_attempts(
                    state.as_ref(),
                    workspace_id,
                    payload.project_id,
                    &document_for_processing,
                    runtime_ingestion_run_id,
                    attempt_no,
                    chunk.id,
                    document_context.target_revision_id.or(document.current_revision_id),
                    &outcome.recovery_attempts,
                )
                .await?;
                outcome
            }
            Err(error) => {
                persist_graph_extraction_recovery_attempts(
                    state.as_ref(),
                    workspace_id,
                    payload.project_id,
                    &document_for_processing,
                    runtime_ingestion_run_id,
                    attempt_no,
                    chunk.id,
                    document_context.target_revision_id.or(document.current_revision_id),
                    &error.recovery_attempts,
                )
                .await?;
                if let (Some(run_id), Some(provider_failure)) =
                    (runtime_ingestion_run_id, &error.provider_failure)
                {
                    repositories::record_runtime_graph_progress_failure_classification(
                        &state.persistence.postgres,
                        run_id,
                        attempt_no,
                        Some(match provider_failure.failure_class {
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InternalRequestInvalid => "internal_request_invalid",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamProtocolFailure => "upstream_protocol_failure",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamTimeout => "upstream_timeout",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamRejection => "upstream_rejection",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InvalidModelOutput => "invalid_model_output",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::RecoveredAfterRetry => "recovered_after_retry",
                        }),
                        Some(&error.request_shape_key),
                        i64::try_from(error.request_size_bytes).ok(),
                        provider_failure.upstream_status.as_deref(),
                        provider_failure.retry_decision.as_deref(),
                    )
                    .await?;
                }
                if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
                    let provider_failure_json = error
                        .provider_failure
                        .clone()
                        .and_then(|value| serde_json::to_value(value).ok());
                    let provider_failure_class = error.provider_failure.as_ref().map(|detail| {
                        match detail.failure_class {
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InternalRequestInvalid => "internal_request_invalid",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamProtocolFailure => "upstream_protocol_failure",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamTimeout => "upstream_timeout",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamRejection => "upstream_rejection",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InvalidModelOutput => "invalid_model_output",
                            crate::domains::runtime_ingestion::RuntimeProviderFailureClass::RecoveredAfterRetry => "recovered_after_retry",
                        }
                    });
                    let replay_count =
                        i32::try_from(error.resume_state.replay_count).unwrap_or(i32::MAX);
                    let downgrade_level =
                        i32::try_from(error.resume_state.downgrade_level).unwrap_or(i32::MAX);
                    let resume_row = repositories::upsert_runtime_graph_extraction_resume_state(
                        &state.persistence.postgres,
                        &repositories::UpsertRuntimeGraphExtractionResumeStateInput {
                            ingestion_run_id: runtime_ingestion_run_id,
                            chunk_ordinal: chunk.ordinal,
                            chunk_content_hash: chunk_content_hash.clone(),
                            status: "failed".to_string(),
                            last_attempt_no: attempt_no,
                            replay_count,
                            resume_hit_count: graph_resume_rows_by_ordinal
                                .get(&chunk.ordinal)
                                .map(|row| row.resume_hit_count)
                                .unwrap_or(0),
                            downgrade_level,
                            provider_kind: error
                                .provider_failure
                                .as_ref()
                                .and_then(|value| value.provider_kind.clone()),
                            model_name: error
                                .provider_failure
                                .as_ref()
                                .and_then(|value| value.model_name.clone()),
                            prompt_hash: None,
                            request_shape_key: Some(error.request_shape_key.clone()),
                            request_size_bytes: i64::try_from(error.request_size_bytes).ok(),
                            provider_failure_class: provider_failure_class.map(str::to_string),
                            provider_failure_json,
                            recovery_summary_json: serde_json::to_value(&error.recovery_summary)
                                .unwrap_or_else(|_| serde_json::json!({})),
                            raw_output_json: serde_json::json!({}),
                            normalized_output_json: serde_json::json!({ "entities": [], "relations": [] }),
                            last_successful_at: None,
                        },
                    )
                    .await?;
                    graph_resume_rows_by_ordinal.insert(chunk.ordinal, resume_row);
                }
                return Err(anyhow::anyhow!(error.to_string()));
            }
        };
        if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
            let provider_failure_json = extracted
                .provider_failure
                .clone()
                .and_then(|value| serde_json::to_value(value).ok());
            let provider_failure_class = extracted.provider_failure.as_ref().map(|detail| {
                match detail.failure_class {
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InternalRequestInvalid => "internal_request_invalid",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamProtocolFailure => "upstream_protocol_failure",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamTimeout => "upstream_timeout",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamRejection => "upstream_rejection",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InvalidModelOutput => "invalid_model_output",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::RecoveredAfterRetry => "recovered_after_retry",
                }
            });
            let resume_row = repositories::upsert_runtime_graph_extraction_resume_state(
                &state.persistence.postgres,
                &repositories::UpsertRuntimeGraphExtractionResumeStateInput {
                    ingestion_run_id: runtime_ingestion_run_id,
                    chunk_ordinal: chunk.ordinal,
                    chunk_content_hash: chunk_content_hash.clone(),
                    status: "ready".to_string(),
                    last_attempt_no: attempt_no,
                    replay_count: i32::try_from(extracted.resume_state.replay_count)
                        .unwrap_or(i32::MAX),
                    resume_hit_count: graph_resume_rows_by_ordinal
                        .get(&chunk.ordinal)
                        .map(|row| row.resume_hit_count)
                        .unwrap_or(0),
                    downgrade_level: i32::try_from(extracted.resume_state.downgrade_level)
                        .unwrap_or(i32::MAX),
                    provider_kind: Some(extracted.provider_kind.clone()),
                    model_name: Some(extracted.model_name.clone()),
                    prompt_hash: Some(extracted.prompt_hash.clone()),
                    request_shape_key: Some(extracted.request_shape_key.clone()),
                    request_size_bytes: i64::try_from(extracted.request_size_bytes).ok(),
                    provider_failure_class: provider_failure_class.map(str::to_string),
                    provider_failure_json,
                    recovery_summary_json: serde_json::to_value(&extracted.recovery_summary)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    raw_output_json: extracted.raw_output_json.clone(),
                    normalized_output_json: serde_json::to_value(&extracted.normalized)
                        .unwrap_or_else(|_| serde_json::json!({ "entities": [], "relations": [] })),
                    last_successful_at: Some(Utc::now()),
                },
            )
            .await?;
            graph_resume_rows_by_ordinal.insert(chunk.ordinal, resume_row);
        }
        if let (Some(run_id), Some(provider_failure)) =
            (runtime_ingestion_run_id, &extracted.provider_failure)
        {
            repositories::record_runtime_graph_progress_failure_classification(
                &state.persistence.postgres,
                run_id,
                attempt_no,
                Some(match provider_failure.failure_class {
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InternalRequestInvalid => "internal_request_invalid",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamProtocolFailure => "upstream_protocol_failure",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamTimeout => "upstream_timeout",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::UpstreamRejection => "upstream_rejection",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::InvalidModelOutput => "invalid_model_output",
                    crate::domains::runtime_ingestion::RuntimeProviderFailureClass::RecoveredAfterRetry => "recovered_after_retry",
                }),
                Some(&extracted.request_shape_key),
                i64::try_from(extracted.request_size_bytes).ok(),
                provider_failure.upstream_status.as_deref(),
                provider_failure.retry_decision.as_deref(),
            )
            .await?;
        }
        if let (Some(runtime_ingestion_run_id), Some(extracting_graph_span)) =
            (runtime_ingestion_run_id, extracting_graph_span.as_ref())
        {
            for usage_call in &extracted.usage_calls {
                graph_extract_call_sequence_no = graph_extract_call_sequence_no.saturating_add(1);
                let _ = document_accounting::record_stage_usage_and_cost(
                    state.as_ref(),
                    document_accounting::StageUsageAccountingRequest {
                        ingestion_run_id: runtime_ingestion_run_id,
                        stage_event_id: extracting_graph_span.stage_event_id,
                        stage: "extracting_graph".to_string(),
                        accounting_scope: document_accounting::StageAccountingScope::ProviderCall {
                            call_sequence_no: graph_extract_call_sequence_no,
                        },
                        workspace_id,
                        project_id: Some(payload.project_id),
                        model_profile_id: None,
                        provider_kind: extracted.provider_kind.clone(),
                        model_name: extracted.model_name.clone(),
                        capability: PricingCapability::GraphExtract,
                        billing_unit: PricingBillingUnit::Per1MTokens,
                        usage_kind: "runtime_document_graph_extract_call".to_string(),
                        prompt_tokens: usage_call
                            .usage_json
                            .get("prompt_tokens")
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        completion_tokens: usage_call
                            .usage_json
                            .get("completion_tokens")
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        total_tokens: usage_call
                            .usage_json
                            .get("total_tokens")
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        raw_usage_json: serde_json::json!({
                            "provider_call_no": usage_call.provider_call_no,
                            "provider_attempt_no": usage_call.provider_attempt_no,
                            "graph_prompt_hash": usage_call.prompt_hash,
                            "request_shape_key": usage_call.request_shape_key,
                            "request_size_bytes": usage_call.request_size_bytes,
                            "chunk_id": chunk.id,
                            "chunk_ordinal": chunk.ordinal,
                            "document_id": document_for_processing.id,
                            "usage": usage_call.usage_json,
                            "provider_kind": extracted.provider_kind,
                            "model_name": extracted.model_name,
                            "timing": usage_call.timing,
                            "prompt_tokens": usage_call.usage_json.get("prompt_tokens").cloned().unwrap_or(serde_json::Value::Null),
                            "completion_tokens": usage_call.usage_json.get("completion_tokens").cloned().unwrap_or(serde_json::Value::Null),
                            "total_tokens": usage_call.usage_json.get("total_tokens").cloned().unwrap_or(serde_json::Value::Null),
                        }),
                    },
                )
                .await?;
            }
        }
        graph_extract_usage.absorb_usage_json(&extracted.usage_json);
        graph_progress_tracker
            .record_extraction(&summarize_graph_extraction_usage_calls(&extracted.usage_calls));
        if let Err(error) = persist_worker_graph_candidates(
            state.as_ref(),
            workspace_id,
            payload.project_id,
            &document_for_processing,
            &chunk,
            document_context.target_revision_id.or(document.current_revision_id),
            &extracted.normalized,
        )
        .await
        {
            warn!(
                job_id = %job.id,
                %worker_id,
                chunk_id = %chunk.id,
                revision_id = ?document_context.target_revision_id.or(document.current_revision_id),
                error = %error,
                "failed to mirror graph candidates into Arango"
            );
        }
        if !extracted.normalized.entities.is_empty() || !extracted.normalized.relations.is_empty() {
            chunk_graph_results.push((
                chunk.clone(),
                extracted.normalized,
                extracted.recovery_summary,
            ));
        }
        maybe_persist_graph_progress_checkpoint(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            &mut graph_progress_tracker,
            chunk_index + 1,
            persisted_chunks.len(),
        )
        .await?;
    }
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        let extracting_graph_event = complete_runtime_stage(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            extracting_graph_span
                .as_ref()
                .expect("runtime stage span must exist when runtime run id exists"),
            Some(extracting_graph_completed_message(rebuild_follow_up)),
            job.id,
        )
        .await?;
        maybe_record_usage_stage_accounting(
            state.as_ref(),
            workspace_id,
            payload.project_id,
            runtime_ingestion_run_id,
            "extracting_graph",
            &extracting_graph_event,
            PricingCapability::GraphExtract,
            PricingBillingUnit::Per1MTokens,
            "runtime_document_graph_extract",
            None,
            &graph_extract_usage,
        )
        .await?;
        upsert_runtime_document_graph_contribution_summary(
            state.as_ref(),
            payload.project_id,
            document.id,
            document_context.target_revision_id.or(document.current_revision_id),
            runtime_ingestion_run_id,
            attempt_no,
        )
        .await?;
    }

    let merging_graph_span = start_runtime_stage(
        state.as_ref(),
        runtime_ingestion_run_id,
        attempt_no,
        "merging_graph",
        Some(MERGING_GRAPH_PROGRESS_START_PERCENT),
        Some(merging_graph_stage_message(rebuild_follow_up)),
        job.id,
        None,
        None,
    )
    .await?;
    let merge_scope = GraphMergeScope::new(payload.project_id, projection_scope.projection_version)
        .with_lifecycle(
            document_context.target_revision_id.or(document.current_revision_id),
            runtime_ingestion_run_id,
        );
    let mut graph_contribution_count = 0usize;
    let mut merge_follow_up_required = false;
    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();
    for (chunk, normalized, recovery_summary) in &chunk_graph_results {
        let merge_outcome = merge_chunk_graph_candidates(
            &state.persistence.postgres,
            &state.bulk_ingest_hardening_services.graph_quality_guard,
            &merge_scope,
            &document_for_processing,
            chunk,
            normalized,
            Some(recovery_summary),
        )
        .await?;
        merge_follow_up_required |= merge_outcome.has_projection_follow_up();
        graph_contribution_count += merge_outcome.nodes.len() + merge_outcome.edges.len();
        changed_node_ids.extend(merge_outcome.summary_refresh_node_ids());
        changed_edge_ids.extend(merge_outcome.summary_refresh_edge_ids());
    }
    reconcile_merge_support_counts(
        &state.persistence.postgres,
        &merge_scope,
        &changed_node_ids.iter().copied().collect::<Vec<_>>(),
        &changed_edge_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await?;

    let changed_edge_rows = repositories::list_admitted_runtime_graph_edges_by_ids(
        &state.persistence.postgres,
        payload.project_id,
        projection_scope.projection_version,
        &changed_edge_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await
    .context("failed to load changed graph edges after merge stage")?;
    let changed_node_rows = repositories::list_admitted_runtime_graph_nodes_by_ids(
        &state.persistence.postgres,
        payload.project_id,
        projection_scope.projection_version,
        &changed_node_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await
    .context("failed to load changed graph nodes after merge stage")?;
    let canonical_graph_ready = match persist_worker_graph_truth(
        state.as_ref(),
        workspace_id,
        payload.project_id,
        &document_for_processing,
        document_context.target_revision_id.or(document.current_revision_id),
        projection_scope.projection_version,
        &changed_node_rows,
        &changed_edge_rows,
    )
    .await
    {
        Ok(ready) => ready,
        Err(error) => {
            warn!(
                job_id = %job.id,
                %worker_id,
                project_id = %payload.project_id,
                error = %error,
                "failed to mirror canonical graph truth into Arango"
            );
            false
        }
    };

    if merge_follow_up_required {
        let supporting_node_rows = if changed_edge_rows.is_empty() {
            Vec::new()
        } else {
            let supporting_node_ids =
                collect_graph_embedding_support_node_ids(&changed_node_ids, &changed_edge_rows);
            repositories::list_admitted_runtime_graph_nodes_by_ids(
                &state.persistence.postgres,
                payload.project_id,
                projection_scope.projection_version,
                &supporting_node_ids,
            )
            .await
            .context("failed to load supporting graph nodes after merge stage")?
        };
        if !changed_node_rows.is_empty() {
            let _node_embedding_usage = embed_runtime_graph_nodes(
                state.as_ref(),
                &provider_profile,
                &changed_node_rows,
                Some(&mut lease_heartbeat),
            )
            .await?;
        }
        if !changed_edge_rows.is_empty() {
            let _edge_embedding_usage = embed_runtime_graph_edges(
                state.as_ref(),
                &provider_profile,
                &supporting_node_rows,
                &changed_edge_rows,
                Some(&mut lease_heartbeat),
            )
            .await?;
        }
    }
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        let _merging_graph_event = complete_runtime_stage(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            merging_graph_span
                .as_ref()
                .expect("runtime stage span must exist when runtime run id exists"),
            Some(merging_graph_completed_message(rebuild_follow_up)),
            job.id,
        )
        .await?;
    }

    let projecting_graph_span = start_runtime_stage(
        state.as_ref(),
        runtime_ingestion_run_id,
        attempt_no,
        "projecting_graph",
        Some(95),
        Some(projecting_graph_stage_message(rebuild_follow_up)),
        job.id,
        None,
        None,
    )
    .await?;
    let projection_outcome = if is_revision_update_mutation(&payload) {
        let summary_refresh = if changed_node_ids.is_empty() && changed_edge_ids.is_empty() {
            GraphSummaryRefreshRequest::broad()
        } else {
            GraphSummaryRefreshRequest::targeted(
                changed_node_ids.iter().copied().collect(),
                changed_edge_ids.iter().copied().collect(),
            )
        };
        finalize_revision_mutation(
            state.as_ref(),
            &payload,
            &document_context,
            &document_for_processing,
            &checksum,
            &projection_scope,
            summary_refresh,
        )
        .await?
    } else {
        let source_truth_version =
            invalidate_library_source_truth(state.as_ref(), payload.project_id)
                .await
                .context("failed to advance project source truth after document upload")?;
        let summary_refresh = if changed_node_ids.is_empty() && changed_edge_ids.is_empty() {
            GraphSummaryRefreshRequest::broad()
        } else {
            GraphSummaryRefreshRequest::targeted(
                changed_node_ids.iter().copied().collect(),
                changed_edge_ids.iter().copied().collect(),
            )
        }
        .with_source_truth_version(source_truth_version);
        let projection_scope = projection_scope.clone().with_summary_refresh(summary_refresh);
        let existing_snapshot = repositories::get_runtime_graph_snapshot(
            &state.persistence.postgres,
            payload.project_id,
        )
        .await
        .context("failed to load graph snapshot after merge stage")?;
        let graph_is_empty = existing_snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.node_count <= 0 && snapshot.edge_count <= 0);

        if graph_contribution_count > 0 || graph_is_empty {
            project_canonical_graph(state.as_ref(), &projection_scope).await?
        } else {
            let snapshot = existing_snapshot.expect("snapshot must exist when graph is not empty");
            repositories::upsert_runtime_graph_snapshot(
                &state.persistence.postgres,
                payload.project_id,
                "ready",
                projection_scope.projection_version,
                snapshot.node_count,
                snapshot.edge_count,
                Some(100.0),
                None,
            )
            .await?;
            crate::services::graph_projection::GraphProjectionOutcome {
                projection_version: projection_scope.projection_version,
                node_count: usize::try_from(snapshot.node_count).unwrap_or_default(),
                edge_count: usize::try_from(snapshot.edge_count).unwrap_or_default(),
                graph_status: "ready".to_string(),
            }
        }
    };
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        let projection_stage_status =
            if projection_outcome.graph_status == "ready" { "completed" } else { "skipped" };
        complete_runtime_stage_with_status(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            projecting_graph_span
                .as_ref()
                .expect("runtime stage span must exist when runtime run id exists"),
            projection_stage_status,
            Some(projecting_graph_completed_message(
                rebuild_follow_up,
                &projection_outcome.graph_status,
            )),
            job.id,
        )
        .await?;
    }

    repositories::mark_ingestion_job_stage(
        &state.persistence.postgres,
        job.id,
        worker_id,
        "running",
        "finalizing",
        None,
    )
    .await?;
    repositories::mark_ingestion_job_attempt_stage(
        &state.persistence.postgres,
        job.id,
        attempt_no,
        worker_id,
        "running",
        "finalizing",
        None,
    )
    .await?;
    let finalizing_span = start_runtime_stage(
        state.as_ref(),
        runtime_ingestion_run_id,
        attempt_no,
        "finalizing",
        Some(99),
        Some(finalizing_stage_message(rebuild_follow_up)),
        job.id,
        None,
        None,
    )
    .await?;
    let terminal_status = if canonical_graph_ready && projection_outcome.graph_status == "ready" {
        "ready"
    } else {
        "ready_no_graph"
    };

    repositories::complete_ingestion_job(
        &state.persistence.postgres,
        job.id,
        worker_id,
        serde_json::json!({
            "document_id": document.id,
            "chunk_count": chunk_count,
            "checksum": checksum,
            "attempt_no": attempt_no,
            "runtime_ingestion_run_id": runtime_ingestion_run_id,
            "graph_contribution_count": graph_contribution_count,
            "projection_version": projection_scope.projection_version,
            "terminal_status": terminal_status,
        }),
    )
    .await?;
    repositories::complete_ingestion_job_attempt(
        &state.persistence.postgres,
        job.id,
        attempt_no,
        worker_id,
        "completed",
    )
    .await?;
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        repositories::update_runtime_ingestion_run_status(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
            terminal_status,
            "finalizing",
            Some(100),
            None,
        )
        .await?;
        complete_runtime_stage(
            state.as_ref(),
            runtime_ingestion_run_id,
            attempt_no,
            finalizing_span
                .as_ref()
                .expect("runtime stage span must exist when runtime run id exists"),
            Some(finalizing_completed_message(rebuild_follow_up, terminal_status)),
            job.id,
        )
        .await?;
    }
    finalize_document_attempt_success(state.as_ref(), &payload, &document_context, terminal_status)
        .await?;

    info!(
        job_id = %job.id,
        %worker_id,
        document_id = %document.id,
        chunk_count,
        elapsed_ms = started_at.elapsed().as_millis(),
        "completed ingestion job",
    );
    Ok(())
}

async fn ensure_worker_document(
    state: &AppState,
    workspace_id: Option<Uuid>,
    payload: &repositories::IngestionExecutionPayload,
    runtime_ingestion_run_id: Option<Uuid>,
    existing_document: Option<repositories::DocumentRow>,
    previous_active_revision: Option<repositories::DocumentRevisionRow>,
    checksum: &str,
    text_len: usize,
) -> anyhow::Result<WorkerDocumentContext> {
    if let Some(document) = existing_document {
        let target_revision_id =
            payload.target_revision_id.or(document.current_revision_id).with_context(|| {
                format!(
                    "document {} is missing a target or active revision during worker sync",
                    document.id
                )
            })?;
        let target_revision = repositories::get_document_revision_by_id(
            &state.persistence.postgres,
            target_revision_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to load target revision {} for document {}",
                target_revision_id, document.id
            )
        })?
        .with_context(|| {
            format!("target revision {} for document {} not found", target_revision_id, document.id)
        })?;
        let old_chunk_ids =
            repositories::list_chunks_by_document(&state.persistence.postgres, document.id)
                .await
                .with_context(|| {
                    format!("failed to load existing chunks for document {}", document.id)
                })?
                .into_iter()
                .map(|chunk| chunk.id)
                .collect::<Vec<_>>();
        cleanup_retry_attempt_artifacts(
            state,
            payload,
            &document,
            previous_active_revision.as_ref(),
            &old_chunk_ids,
        )
        .await?;
        let document_for_processing = build_processing_document(&document, payload, checksum);
        if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
            repositories::attach_runtime_ingestion_run_document(
                &state.persistence.postgres,
                runtime_ingestion_run_id,
                document.id,
                Some(target_revision.id),
            )
            .await?;
            persist_extracted_content_from_payload(
                state,
                runtime_ingestion_run_id,
                Some(document.id),
                payload,
            )
            .await?;
        }
        sync_worker_knowledge_document(
            state,
            workspace_id,
            payload,
            &document,
            &target_revision,
            checksum,
            text_len,
        )
        .await?;
        return Ok(WorkerDocumentContext {
            document,
            document_for_processing,
            target_revision_id: Some(target_revision.id),
            target_revision: Some(target_revision),
            previous_active_revision,
            old_chunk_ids,
        });
    }

    let document = repositories::create_document(
        &state.persistence.postgres,
        payload.project_id,
        payload.source_id,
        &payload.external_key,
        payload.title.as_deref(),
        payload.mime_type.as_deref(),
        Some(checksum),
    )
    .await?;
    let target_revision =
        create_initial_document_revision(state, &document, payload, checksum).await?;
    repositories::activate_document_revision(
        &state.persistence.postgres,
        document.id,
        target_revision.id,
    )
    .await
    .with_context(|| format!("failed to activate initial revision {}", target_revision.id))?;
    let document = repositories::update_document_current_revision(
        &state.persistence.postgres,
        document.id,
        Some(target_revision.id),
        "processing",
        payload.mutation_kind.as_deref(),
        payload.mutation_kind.as_deref().map(|_| "reconciling"),
    )
    .await
    .with_context(|| {
        format!("failed to update logical document {} current revision", document.id)
    })?;
    let target_revision =
        repositories::get_document_revision_by_id(&state.persistence.postgres, target_revision.id)
            .await
            .with_context(|| {
                format!(
                    "failed to reload initial revision {} for document {}",
                    target_revision.id, document.id
                )
            })?
            .with_context(|| {
                format!(
                    "initial revision {} for document {} not found",
                    target_revision.id, document.id
                )
            })?;
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        repositories::attach_runtime_ingestion_run_document(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
            document.id,
            Some(target_revision.id),
        )
        .await?;
        persist_extracted_content_from_payload(
            state,
            runtime_ingestion_run_id,
            Some(document.id),
            payload,
        )
        .await?;
    }
    sync_worker_knowledge_document(
        state,
        workspace_id,
        payload,
        &document,
        &target_revision,
        checksum,
        text_len,
    )
    .await?;

    Ok(WorkerDocumentContext {
        document: document.clone(),
        document_for_processing: document,
        target_revision_id: Some(target_revision.id),
        target_revision: Some(target_revision),
        previous_active_revision: None,
        old_chunk_ids: Vec::new(),
    })
}

async fn sync_worker_knowledge_document(
    state: &AppState,
    workspace_id: Option<Uuid>,
    payload: &repositories::IngestionExecutionPayload,
    document: &repositories::DocumentRow,
    target_revision: &repositories::DocumentRevisionRow,
    checksum: &str,
    text_len: usize,
) -> anyhow::Result<()> {
    let workspace_id = workspace_id.with_context(|| {
        format!(
            "missing workspace for project {} while syncing knowledge document {}",
            payload.project_id, document.id
        )
    })?;
    let active_revision_id = document.current_revision_id.or(Some(target_revision.id));
    let latest_revision_no = i64::from(target_revision.revision_no);
    state
        .canonical_services
        .knowledge
        .create_document_shell(
            state,
            CreateKnowledgeDocumentCommand {
                document_id: document.id,
                workspace_id,
                library_id: payload.project_id,
                external_key: document.external_key.clone(),
                title: None,
                document_state: document.active_status.clone(),
            },
        )
        .await
        .with_context(|| format!("failed to sync knowledge document shell {}", document.id))?;
    state
        .canonical_services
        .knowledge
        .write_revision(
            state,
            CreateKnowledgeRevisionCommand {
                revision_id: target_revision.id,
                workspace_id,
                library_id: payload.project_id,
                document_id: document.id,
                revision_number: i64::from(target_revision.revision_no),
                revision_state: target_revision.status.clone(),
                revision_kind: target_revision.revision_kind.clone(),
                storage_ref: None,
                source_uri: None,
                mime_type: target_revision
                    .mime_type
                    .clone()
                    .or_else(|| document.mime_type.clone())
                    .or_else(|| payload.mime_type.clone())
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                checksum: checksum.to_string(),
                byte_size: target_revision
                    .file_size_bytes
                    .or_else(|| payload.file_size_bytes.and_then(|value| i64::try_from(value).ok()))
                    .unwrap_or_else(|| i64::try_from(text_len).unwrap_or(i64::MAX)),
                title: document.title.clone().or_else(|| payload.title.clone()),
                normalized_text: None,
                text_checksum: Some(checksum.to_string()),
                text_state: "ready".to_string(),
                vector_state: "processing".to_string(),
                graph_state: "processing".to_string(),
                text_readable_at: Some(Utc::now()),
                vector_ready_at: None,
                graph_ready_at: None,
                superseded_by_revision_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to sync knowledge revision {}", target_revision.id))?;
    state
        .canonical_services
        .knowledge
        .set_revision_text_state(
            state,
            target_revision.id,
            "ready",
            None,
            Some(checksum),
            Some(Utc::now()),
        )
        .await
        .with_context(|| {
            format!("failed to mark knowledge revision {} as text-ready", target_revision.id)
        })?;
    state
        .canonical_services
        .knowledge
        .promote_document(
            state,
            PromoteKnowledgeDocumentCommand {
                document_id: document.id,
                document_state: document.active_status.clone(),
                active_revision_id,
                readable_revision_id: active_revision_id,
                latest_revision_no: Some(latest_revision_no),
                deleted_at: document.deleted_at,
            },
        )
        .await
        .with_context(|| format!("failed to sync knowledge document pointers {}", document.id))?;

    Ok(())
}

async fn persist_worker_chunk_knowledge(
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Uuid,
    revision: &repositories::DocumentRevisionRow,
    chunk: &repositories::ChunkRow,
    chunk_state: &str,
    vector_generation: Option<i64>,
) -> anyhow::Result<()> {
    let workspace_id = workspace_id.with_context(|| {
        format!(
            "missing workspace for project {} while syncing knowledge chunk {}",
            library_id, chunk.id
        )
    })?;
    let normalized_text = chunk.content.split_whitespace().collect::<Vec<_>>().join(" ");
    state
        .canonical_services
        .knowledge
        .write_chunk(
            state,
            CreateKnowledgeChunkCommand {
                chunk_id: chunk.id,
                workspace_id,
                library_id,
                document_id: chunk.document_id,
                revision_id: revision.id,
                chunk_index: chunk.ordinal,
                content_text: chunk.content.clone(),
                normalized_text,
                span_start: None,
                span_end: None,
                token_count: chunk.token_count,
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                chunk_state: chunk_state.to_string(),
                text_generation: Some(i64::from(revision.revision_no)),
                vector_generation,
            },
        )
        .await
        .with_context(|| format!("failed to sync knowledge chunk {}", chunk.id))?;
    Ok(())
}

async fn persist_worker_chunk_vectors(
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Uuid,
    revision: &repositories::DocumentRevisionRow,
    revision_generation: i64,
    chunks: &[repositories::ChunkRow],
) -> anyhow::Result<usize> {
    let workspace_id = workspace_id.with_context(|| {
        format!(
            "missing workspace for project {} while syncing chunk vectors for revision {}",
            library_id, revision.id
        )
    })?;
    let mut written = 0usize;
    for chunk in chunks {
        let vector_rows =
            state.arango_search_store.list_chunk_vectors_by_chunk(chunk.id).await.with_context(
                || format!("failed to load canonical chunk vectors for {}", chunk.id),
            )?;
        let Some(vector_row) =
            state.canonical_services.search.select_current_chunk_vector(&vector_rows)
        else {
            warn!(
                chunk_id = %chunk.id,
                revision_id = %revision.id,
                "no canonical chunk vector found after chunk embedding stage"
            );
            continue;
        };
        persist_worker_chunk_knowledge(
            state,
            Some(workspace_id),
            library_id,
            revision,
            chunk,
            "ready",
            Some(vector_row.freshness_generation.max(revision_generation)),
        )
        .await?;
        written += 1;
    }

    if written > 0 && written == chunks.len() {
        if let Some(existing_revision) = state
            .arango_document_store
            .get_revision(revision.id)
            .await
            .with_context(|| format!("failed to load knowledge revision {}", revision.id))?
        {
            let _ = state
                .arango_document_store
                .update_revision_readiness(
                    revision.id,
                    &existing_revision.text_state,
                    "ready",
                    &existing_revision.graph_state,
                    existing_revision.text_readable_at,
                    Some(Utc::now()),
                    existing_revision.graph_ready_at,
                    existing_revision.superseded_by_revision_id,
                )
                .await
                .with_context(|| {
                    format!("failed to mark knowledge revision {} as vector-ready", revision.id)
                })?;
        }
    }

    Ok(written)
}

async fn persist_worker_graph_candidates(
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Uuid,
    document: &repositories::DocumentRow,
    chunk: &repositories::ChunkRow,
    revision_id: Option<Uuid>,
    candidates: &GraphExtractionCandidateSet,
) -> anyhow::Result<usize> {
    let workspace_id = workspace_id.with_context(|| {
        format!(
            "missing workspace for project {} while mirroring graph candidates for chunk {}",
            library_id, chunk.id
        )
    })?;
    let revision_id = revision_id.with_context(|| {
        format!("missing revision while mirroring graph candidates for chunk {}", chunk.id)
    })?;
    let extraction_method = "graph_extract".to_string();
    let mut written = 0usize;

    for entity in &candidates.entities {
        let normalization_key = crate::services::graph_merge::canonical_node_key(
            entity.node_type.clone(),
            &entity.label,
        );
        let candidate_id = stable_uuid(&format!(
            "arango-entity-candidate:{library_id}:{revision_id}:{}:{}:{}:{}",
            chunk.id,
            normalization_key,
            entity.label,
            worker_runtime_node_type_slug(&entity.node_type)
        ));
        state
            .arango_graph_store
            .upsert_entity_candidate(
                &crate::infra::arangodb::graph_store::NewKnowledgeEntityCandidate {
                    candidate_id,
                    workspace_id,
                    library_id,
                    revision_id,
                    chunk_id: Some(chunk.id),
                    candidate_label: entity.label.clone(),
                    candidate_type: worker_runtime_node_type_slug(&entity.node_type).to_string(),
                    normalization_key,
                    confidence: None,
                    extraction_method: extraction_method.clone(),
                    candidate_state: "active".to_string(),
                    created_at: Some(Utc::now()),
                    updated_at: Some(Utc::now()),
                },
            )
            .await
            .with_context(|| {
                format!(
                    "failed to upsert knowledge entity candidate for document {} chunk {}",
                    document.id, chunk.id
                )
            })?;
        written += 1;
    }

    for relation in &candidates.relations {
        let source_normalization_key = crate::services::graph_merge::canonical_node_key(
            RuntimeNodeType::Entity,
            &relation.source_label,
        );
        let target_normalization_key = crate::services::graph_merge::canonical_node_key(
            RuntimeNodeType::Entity,
            &relation.target_label,
        );
        let normalized_assertion = crate::services::graph_merge::canonical_edge_key(
            &source_normalization_key,
            &relation.relation_type,
            &target_normalization_key,
        );
        let candidate_id = stable_uuid(&format!(
            "arango-relation-candidate:{library_id}:{revision_id}:{}:{normalized_assertion}:{}:{}:{}",
            chunk.id, relation.source_label, relation.target_label, relation.relation_type
        ));
        state
            .arango_graph_store
            .upsert_relation_candidate(
                &crate::infra::arangodb::graph_store::NewKnowledgeRelationCandidate {
                    candidate_id,
                    workspace_id,
                    library_id,
                    revision_id,
                    chunk_id: Some(chunk.id),
                    subject_candidate_key: source_normalization_key,
                    predicate: relation.relation_type.clone(),
                    object_candidate_key: target_normalization_key,
                    normalized_assertion,
                    confidence: None,
                    extraction_method: extraction_method.clone(),
                    candidate_state: "active".to_string(),
                    created_at: Some(Utc::now()),
                    updated_at: Some(Utc::now()),
                },
            )
            .await
            .with_context(|| {
                format!(
                    "failed to upsert knowledge relation candidate for document {} chunk {}",
                    document.id, chunk.id
                )
            })?;
        written += 1;
    }

    Ok(written)
}

async fn persist_worker_graph_truth(
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Uuid,
    document: &repositories::DocumentRow,
    revision_id: Option<Uuid>,
    projection_version: i64,
    changed_node_rows: &[repositories::RuntimeGraphNodeRow],
    changed_edge_rows: &[repositories::RuntimeGraphEdgeRow],
) -> anyhow::Result<bool> {
    let workspace_id = workspace_id.with_context(|| {
        format!(
            "missing workspace for project {} while mirroring canonical graph truth",
            library_id
        )
    })?;
    let revision_id = revision_id.with_context(|| {
        format!(
            "missing revision while mirroring canonical graph truth for document {}",
            document.id
        )
    })?;
    let existing_generation = state
        .arango_document_store
        .list_library_generations(library_id)
        .await
        .ok()
        .and_then(|rows| rows.into_iter().next());

    let mut node_rows_by_id = BTreeMap::<Uuid, &repositories::RuntimeGraphNodeRow>::new();
    let mut edge_rows_by_id = BTreeMap::<Uuid, &repositories::RuntimeGraphEdgeRow>::new();
    for row in changed_node_rows {
        node_rows_by_id.insert(row.id, row);
    }
    for row in changed_edge_rows {
        edge_rows_by_id.insert(row.id, row);
    }

    let mut entity_id_by_runtime_node_id = BTreeMap::<Uuid, Uuid>::new();
    let mut relation_id_by_runtime_edge_id = BTreeMap::<Uuid, Uuid>::new();
    let mut mirrored_anything = false;

    for row in changed_node_rows {
        let Some(node_type) = runtime_node_type_from_slug(&row.node_type) else {
            warn!(
                node_id = %row.id,
                node_type = %row.node_type,
                "skipping runtime node with unsupported type while mirroring canonical graph truth"
            );
            continue;
        };
        let aliases: Vec<String> = serde_json::from_value(row.aliases_json.clone())
            .unwrap_or_else(|_| vec![row.label.clone()]);
        let entity_id = stable_uuid(&format!(
            "arango-entity:{library_id}:{}:{}",
            row.node_type, row.canonical_key
        ));
        let entity = state
            .arango_graph_store
            .upsert_entity(&crate::infra::arangodb::graph_store::NewKnowledgeEntity {
                entity_id,
                workspace_id,
                library_id,
                canonical_label: row.label.clone(),
                aliases: {
                    let mut values = aliases;
                    if !values.iter().any(|value| value == &row.label) {
                        values.push(row.label.clone());
                    }
                    values.sort();
                    values.dedup();
                    values
                },
                entity_type: row.node_type.clone(),
                summary: row.summary.clone(),
                confidence: None,
                support_count: i64::from(row.support_count),
                freshness_generation: projection_version,
                entity_state: "active".to_string(),
                created_at: Some(row.created_at),
                updated_at: Some(Utc::now()),
            })
            .await
            .with_context(|| format!("failed to mirror canonical entity {} into Arango", row.id))?;
        mirrored_anything = true;
        entity_id_by_runtime_node_id.insert(row.id, entity.entity_id);
        if matches!(node_type, RuntimeNodeType::Entity | RuntimeNodeType::Topic) {
            mirrored_anything = true;
        }
    }

    for row in changed_edge_rows {
        if row.relation_type.trim().is_empty() {
            warn!(
                edge_id = %row.id,
                "skipping runtime edge with empty relation type while mirroring canonical graph truth"
            );
            continue;
        }
        let subject_entity_id = entity_id_by_runtime_node_id.get(&row.from_node_id).copied();
        let object_entity_id = entity_id_by_runtime_node_id.get(&row.to_node_id).copied();
        let relation_id =
            stable_uuid(&format!("arango-relation:{library_id}:{}", row.canonical_key));
        let relation = state
            .arango_graph_store
            .upsert_relation_with_endpoints(
                &crate::infra::arangodb::graph_store::NewKnowledgeRelation {
                    relation_id,
                    workspace_id,
                    library_id,
                    predicate: row.relation_type.clone(),
                    normalized_assertion: row.canonical_key.clone(),
                    confidence: row.weight,
                    support_count: i64::from(row.support_count),
                    contradiction_state: "unknown".to_string(),
                    freshness_generation: projection_version,
                    relation_state: "active".to_string(),
                    created_at: Some(row.created_at),
                    updated_at: Some(Utc::now()),
                },
                subject_entity_id,
                object_entity_id,
            )
            .await
            .with_context(|| {
                format!("failed to mirror canonical relation {} into Arango", row.id)
            })?;
        mirrored_anything = true;
        relation_id_by_runtime_edge_id.insert(row.id, relation.relation_id);
    }

    let runtime_evidence_rows =
        repositories::list_active_runtime_graph_evidence_by_document_revision(
            &state.persistence.postgres,
            library_id,
            document.id,
            revision_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to list runtime graph evidence for document {} revision {}",
                document.id, revision_id
            )
        })?;
    for row in runtime_evidence_rows {
        let (supporting_entity_id, supporting_relation_id, evidence_key, support_kind) = match row
            .target_kind
            .as_str()
        {
            "node" => {
                let Some(runtime_node) = node_rows_by_id.get(&row.target_id) else {
                    warn!(
                        evidence_id = %row.id,
                        target_id = %row.target_id,
                        "skipping node evidence without a mirrored canonical entity"
                    );
                    continue;
                };
                (
                    entity_id_by_runtime_node_id.get(&runtime_node.id).copied(),
                    None,
                    runtime_node.canonical_key.clone(),
                    "node".to_string(),
                )
            }
            "edge" => {
                let Some(runtime_edge) = edge_rows_by_id.get(&row.target_id) else {
                    warn!(
                        evidence_id = %row.id,
                        target_id = %row.target_id,
                        "skipping edge evidence without a mirrored canonical relation"
                    );
                    continue;
                };
                (
                    None,
                    relation_id_by_runtime_edge_id.get(&runtime_edge.id).copied(),
                    runtime_edge.canonical_key.clone(),
                    "edge".to_string(),
                )
            }
            other => {
                warn!(
                    evidence_id = %row.id,
                    target_kind = other,
                    "skipping unsupported runtime evidence kind while mirroring canonical graph truth"
                );
                continue;
            }
        };

        let evidence_id = stable_uuid(&format!(
            "arango-evidence:{library_id}:{revision_id}:{}:{}:{evidence_key}",
            row.chunk_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
            support_kind,
        ));
        let _ = state
            .arango_graph_store
            .upsert_evidence_with_edges(
                &crate::infra::arangodb::graph_store::NewKnowledgeEvidence {
                    evidence_id,
                    workspace_id,
                    library_id,
                    document_id: document.id,
                    revision_id,
                    chunk_id: row.chunk_id,
                    span_start: None,
                    span_end: None,
                    excerpt: row.evidence_text.clone(),
                    support_kind: support_kind.clone(),
                    extraction_method: "runtime_graph_merge".to_string(),
                    confidence: row.confidence_score,
                    evidence_state: if row.is_active { "active" } else { "inactive" }.to_string(),
                    freshness_generation: projection_version,
                    created_at: Some(row.created_at),
                    updated_at: Some(Utc::now()),
                },
                Some(revision_id),
                supporting_entity_id,
                supporting_relation_id,
            )
            .await
            .with_context(|| {
                format!("failed to mirror canonical evidence {} into Arango", row.id)
            })?;
        mirrored_anything = true;
        if row.target_kind == "node" {
            if let (Some(chunk_id), Some(entity_id)) = (row.chunk_id, supporting_entity_id) {
                state
                    .arango_graph_store
                    .upsert_chunk_mentions_entity_edge(
                        chunk_id,
                        entity_id,
                        None,
                        row.confidence_score,
                        Some("runtime_graph_evidence".to_string()),
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to mirror chunk mentions edge for canonical evidence {}",
                            row.id
                        )
                    })?;
            }
        }
    }

    let current_revision = state
        .arango_document_store
        .get_revision(revision_id)
        .await
        .with_context(|| format!("failed to load knowledge revision {}", revision_id))?
        .with_context(|| {
            format!("knowledge revision {} disappeared during graph mirror", revision_id)
        })?;
    let graph_ready = mirrored_anything;
    let graph_state = if graph_ready { "ready" } else { "processing" };
    let graph_ready_at = graph_ready.then(Utc::now);
    state
        .arango_document_store
        .update_revision_readiness(
            revision_id,
            &current_revision.text_state,
            &current_revision.vector_state,
            graph_state,
            current_revision.text_readable_at,
            current_revision.vector_ready_at,
            graph_ready_at,
            current_revision.superseded_by_revision_id,
        )
        .await
        .with_context(|| {
            format!("failed to update knowledge revision {} graph readiness", revision_id)
        })?;

    let generation_id =
        existing_generation.as_ref().map(|row| row.generation_id).unwrap_or_else(Uuid::now_v7);
    let active_text_generation = if current_revision.text_state == "ready" {
        current_revision.revision_number
    } else {
        existing_generation.as_ref().map(|row| row.active_text_generation).unwrap_or(0)
    };
    let active_vector_generation = if current_revision.vector_state == "ready" {
        current_revision.revision_number
    } else {
        existing_generation.as_ref().map(|row| row.active_vector_generation).unwrap_or(0)
    };
    let active_graph_generation = if graph_ready {
        projection_version
    } else {
        existing_generation.as_ref().map(|row| row.active_graph_generation).unwrap_or(0)
    };
    let degraded_state = if current_revision.text_state == "ready"
        && current_revision.vector_state == "ready"
        && graph_ready
    {
        "ready"
    } else {
        "degraded"
    };
    state
        .canonical_services
        .knowledge
        .refresh_library_generation(
            state,
            RefreshKnowledgeLibraryGenerationCommand {
                generation_id,
                workspace_id,
                library_id,
                active_text_generation,
                active_vector_generation,
                active_graph_generation,
                degraded_state: degraded_state.to_string(),
            },
        )
        .await
        .with_context(|| {
            format!("failed to refresh knowledge library generation for {}", library_id)
        })?;

    Ok(graph_ready)
}

fn stable_uuid(seed: &str) -> Uuid {
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn text_stage_is_ready(state: &str) -> bool {
    matches!(state, "ready" | "text_readable" | "vector_ready" | "graph_ready")
}

fn vector_stage_is_ready(state: &str) -> bool {
    matches!(state, "ready" | "vector_ready" | "graph_ready")
}

fn graph_stage_is_ready(state: &str) -> bool {
    matches!(state, "ready" | "graph_ready")
}

fn preserve_or_fail_stage_state(current: &str, stage_ready: bool) -> String {
    if stage_ready || matches!(current, "failed" | "superseded") {
        current.to_string()
    } else {
        "failed".to_string()
    }
}

fn preserve_failure_revision_truth(
    current: &KnowledgeRevisionRow,
) -> PreservedFailureRevisionTruth {
    let text_ready = current.text_readable_at.is_some() || text_stage_is_ready(&current.text_state);
    let vector_ready =
        current.vector_ready_at.is_some() || vector_stage_is_ready(&current.vector_state);
    let graph_ready =
        current.graph_ready_at.is_some() || graph_stage_is_ready(&current.graph_state);
    PreservedFailureRevisionTruth {
        text_state: preserve_or_fail_stage_state(&current.text_state, text_ready),
        vector_state: preserve_or_fail_stage_state(&current.vector_state, vector_ready),
        graph_state: preserve_or_fail_stage_state(&current.graph_state, graph_ready),
        text_readable_at: current.text_readable_at,
        vector_ready_at: current.vector_ready_at,
        graph_ready_at: current.graph_ready_at,
    }
}

fn build_failure_generation_snapshot(
    existing: Option<&KnowledgeLibraryGenerationRow>,
    revision: &KnowledgeRevisionRow,
    preserved: &PreservedFailureRevisionTruth,
) -> Option<FailureGenerationSnapshot> {
    if existing.is_none()
        && !preserved.text_ready()
        && !preserved.vector_ready()
        && !preserved.graph_ready()
    {
        return None;
    }

    let existing_text_generation = existing.map_or(0, |row| row.active_text_generation);
    let existing_vector_generation = existing.map_or(0, |row| row.active_vector_generation);
    let existing_graph_generation = existing.map_or(0, |row| row.active_graph_generation);
    let active_text_generation = if preserved.text_ready() {
        existing_text_generation.max(revision.revision_number)
    } else {
        existing_text_generation
    };
    let active_vector_generation = if preserved.vector_ready() {
        existing_vector_generation.max(revision.revision_number)
    } else {
        existing_vector_generation
    };
    let active_graph_generation = if preserved.graph_ready() {
        existing_graph_generation.max(revision.revision_number)
    } else {
        existing_graph_generation
    };
    let degraded_state =
        if preserved.text_ready() && preserved.vector_ready() && preserved.graph_ready() {
            "ready"
        } else if preserved.text_ready() || preserved.vector_ready() || preserved.graph_ready() {
            "degraded"
        } else {
            "failed"
        };

    Some(FailureGenerationSnapshot {
        generation_id: existing.map_or_else(Uuid::now_v7, |row| row.generation_id),
        active_text_generation,
        active_vector_generation,
        active_graph_generation,
        degraded_state: degraded_state.to_string(),
    })
}

fn worker_runtime_node_type_slug(node_type: &RuntimeNodeType) -> &'static str {
    match node_type {
        RuntimeNodeType::Document => "document",
        RuntimeNodeType::Entity => "entity",
        RuntimeNodeType::Topic => "topic",
    }
}

fn runtime_node_type_from_slug(node_type: &str) -> Option<RuntimeNodeType> {
    match node_type {
        "document" => Some(RuntimeNodeType::Document),
        "entity" => Some(RuntimeNodeType::Entity),
        "topic" => Some(RuntimeNodeType::Topic),
        _ => None,
    }
}

async fn create_initial_document_revision(
    state: &AppState,
    document: &repositories::DocumentRow,
    payload: &repositories::IngestionExecutionPayload,
    checksum: &str,
) -> anyhow::Result<repositories::DocumentRevisionRow> {
    repositories::create_document_revision(
        &state.persistence.postgres,
        document.id,
        1,
        "initial_upload",
        None,
        &payload.external_key,
        payload.mime_type.as_deref(),
        payload.file_size_bytes.and_then(|value| i64::try_from(value).ok()),
        None,
        Some(checksum),
    )
    .await
    .with_context(|| format!("failed to create initial revision for document {}", document.id))
}

async fn cleanup_retry_attempt_artifacts(
    state: &AppState,
    payload: &repositories::IngestionExecutionPayload,
    document: &repositories::DocumentRow,
    previous_active_revision: Option<&repositories::DocumentRevisionRow>,
    old_chunk_ids: &[Uuid],
) -> anyhow::Result<()> {
    if !should_cleanup_retry_attempt_artifacts(payload, previous_active_revision, old_chunk_ids) {
        return Ok(());
    }

    let mut deleted_query_refs = 0_u64;
    let mut deactivated_evidence = 0_u64;
    if let Some(previous_active_revision) = previous_active_revision {
        deleted_query_refs = repositories::delete_query_execution_references_by_document_revision(
            &state.persistence.postgres,
            payload.project_id,
            document.id,
            previous_active_revision.id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to delete stale retry query references for document {} revision {}",
                document.id, previous_active_revision.id
            )
        })?;
        deactivated_evidence =
            repositories::deactivate_runtime_graph_evidence_by_document_revision(
                &state.persistence.postgres,
                payload.project_id,
                document.id,
                previous_active_revision.id,
                payload.document_mutation_workflow_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to deactivate stale retry graph evidence for document {} revision {}",
                    document.id, previous_active_revision.id
                )
            })?;
    }

    let deleted_chunks =
        repositories::delete_chunks_by_document(&state.persistence.postgres, document.id)
            .await
            .with_context(|| {
                format!("failed to delete stale retry chunks for document {}", document.id)
            })?;

    if let Some(snapshot) =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, payload.project_id)
            .await
            .context("failed to load graph snapshot while cleaning retry artifacts")?
        && snapshot.projection_version > 0
    {
        repositories::recalculate_runtime_graph_support_counts(
            &state.persistence.postgres,
            payload.project_id,
            snapshot.projection_version,
        )
        .await
        .context("failed to recalculate graph support counts after retry cleanup")?;
        repositories::delete_runtime_graph_edges_without_support(
            &state.persistence.postgres,
            payload.project_id,
            snapshot.projection_version,
        )
        .await
        .context("failed to prune unsupported graph edges after retry cleanup")?;
        repositories::delete_runtime_graph_nodes_without_support(
            &state.persistence.postgres,
            payload.project_id,
            snapshot.projection_version,
        )
        .await
        .context("failed to prune unsupported graph nodes after retry cleanup")?;
    }

    if deleted_chunks > 0 || deleted_query_refs > 0 || deactivated_evidence > 0 {
        info!(
            project_id = %payload.project_id,
            document_id = %document.id,
            deleted_chunks,
            deleted_query_refs,
            deactivated_evidence,
            "cleaned stale retry artifacts before replaying document ingestion",
        );
    }

    Ok(())
}

fn should_cleanup_retry_attempt_artifacts(
    payload: &repositories::IngestionExecutionPayload,
    previous_active_revision: Option<&repositories::DocumentRevisionRow>,
    old_chunk_ids: &[Uuid],
) -> bool {
    payload.mutation_kind.is_none()
        && (previous_active_revision.is_some() || !old_chunk_ids.is_empty())
}

fn build_processing_document(
    document: &repositories::DocumentRow,
    payload: &repositories::IngestionExecutionPayload,
    checksum: &str,
) -> repositories::DocumentRow {
    repositories::DocumentRow {
        id: document.id,
        project_id: document.project_id,
        source_id: document.source_id,
        external_key: payload.external_key.clone(),
        title: payload.title.clone().or_else(|| document.title.clone()),
        mime_type: payload.mime_type.clone().or_else(|| document.mime_type.clone()),
        checksum: Some(checksum.to_string()),
        current_revision_id: document.current_revision_id,
        active_status: document.active_status.clone(),
        active_mutation_kind: document.active_mutation_kind.clone(),
        active_mutation_status: document.active_mutation_status.clone(),
        deleted_at: document.deleted_at,
        created_at: document.created_at,
        updated_at: document.updated_at,
    }
}

async fn finalize_revision_mutation(
    state: &AppState,
    payload: &repositories::IngestionExecutionPayload,
    document_context: &WorkerDocumentContext,
    document_for_processing: &repositories::DocumentRow,
    checksum: &str,
    projection_scope: &crate::services::graph_projection::GraphProjectionScope,
    summary_refresh: GraphSummaryRefreshRequest,
) -> anyhow::Result<crate::services::graph_projection::GraphProjectionOutcome> {
    let target_revision_id = document_context.target_revision_id.with_context(|| {
        format!("document {} is missing a target revision", document_context.document.id)
    })?;
    let mut targeted_node_ids = Vec::new();
    let mut targeted_edge_ids = Vec::new();
    let mut effective_summary_refresh = summary_refresh;
    if let (Some(previous_active_revision), Some(mutation_workflow_id)) =
        (document_context.previous_active_revision.as_ref(), payload.document_mutation_workflow_id)
    {
        let detected_scope = state
            .retrieval_intelligence_services
            .graph_reconciliation_scope
            .detect_revision_mutation_scope(
                state,
                payload.project_id,
                document_context.document.id,
                previous_active_revision.id,
                target_revision_id,
            )
            .await
            .context("failed to detect revision-mutation impact scope")?;
        persist_detected_scope(state, mutation_workflow_id, &detected_scope).await?;
        if detected_scope.scope_status == "targeted" {
            targeted_node_ids = detected_scope.affected_node_ids.clone();
            targeted_edge_ids = detected_scope.affected_relationship_ids.clone();
            effective_summary_refresh = GraphSummaryRefreshRequest::targeted(
                targeted_node_ids.clone(),
                targeted_edge_ids.clone(),
            );
        } else {
            effective_summary_refresh = GraphSummaryRefreshRequest::broad();
        }
    }
    repositories::update_document_metadata(
        &state.persistence.postgres,
        document_context.document.id,
        &document_for_processing.external_key,
        document_for_processing.title.as_deref(),
        document_for_processing.mime_type.as_deref(),
        Some(checksum),
    )
    .await
    .with_context(|| {
        format!("failed to update logical document {}", document_context.document.id)
    })?;
    repositories::supersede_document_revisions(
        &state.persistence.postgres,
        document_context.document.id,
        target_revision_id,
    )
    .await
    .with_context(|| {
        format!(
            "failed to supersede previous revisions for document {}",
            document_context.document.id
        )
    })?;
    repositories::activate_document_revision(
        &state.persistence.postgres,
        document_context.document.id,
        target_revision_id,
    )
    .await
    .with_context(|| format!("failed to activate revision {}", target_revision_id))?;
    repositories::update_document_current_revision(
        &state.persistence.postgres,
        document_context.document.id,
        Some(target_revision_id),
        "reconciling",
        payload.mutation_kind.as_deref(),
        payload.mutation_kind.as_deref().map(|_| "reconciling"),
    )
    .await
    .with_context(|| {
        format!(
            "failed to update logical document {} to the new active revision",
            document_context.document.id
        )
    })?;
    if let Some(previous_active_revision) = &document_context.previous_active_revision {
        repositories::delete_query_execution_references_by_document_revision(
            &state.persistence.postgres,
            payload.project_id,
            document_context.document.id,
            previous_active_revision.id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to delete stale query references for document {} revision {}",
                document_context.document.id, previous_active_revision.id
            )
        })?;
        repositories::deactivate_runtime_graph_evidence_by_document_revision(
            &state.persistence.postgres,
            payload.project_id,
            document_context.document.id,
            previous_active_revision.id,
            payload.document_mutation_workflow_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to deactivate stale graph evidence for document {} revision {}",
                document_context.document.id, previous_active_revision.id
            )
        })?;
    }
    repositories::recalculate_runtime_graph_support_counts(
        &state.persistence.postgres,
        payload.project_id,
        projection_scope.projection_version,
    )
    .await
    .context("failed to recalculate graph support counts after revision mutation")?;
    repositories::delete_runtime_graph_edges_without_support(
        &state.persistence.postgres,
        payload.project_id,
        projection_scope.projection_version,
    )
    .await
    .context("failed to prune unsupported graph edges after revision mutation")?;
    repositories::delete_runtime_graph_nodes_without_support(
        &state.persistence.postgres,
        payload.project_id,
        projection_scope.projection_version,
    )
    .await
    .context("failed to prune unsupported graph nodes after revision mutation")?;
    repositories::delete_chunks_by_ids(
        &state.persistence.postgres,
        &document_context.old_chunk_ids,
    )
    .await
    .with_context(|| {
        format!("failed to delete superseded chunks for document {}", document_context.document.id)
    })?;
    let source_truth_version = invalidate_library_source_truth(state, payload.project_id)
        .await
        .context("failed to advance project source truth after revision activation")?;
    let mut projection_scope = projection_scope.clone();
    if !targeted_node_ids.is_empty() || !targeted_edge_ids.is_empty() {
        projection_scope =
            projection_scope.with_targeted_refresh(targeted_node_ids, targeted_edge_ids);
    }
    let projection_scope = projection_scope.with_summary_refresh(
        effective_summary_refresh.with_source_truth_version(source_truth_version),
    );
    project_canonical_graph(state, &projection_scope)
        .await
        .context("failed to project canonical graph after revision mutation")
}

async fn finalize_document_attempt_success(
    state: &AppState,
    payload: &repositories::IngestionExecutionPayload,
    document_context: &WorkerDocumentContext,
    terminal_status: &str,
) -> anyhow::Result<()> {
    if matches!(payload.attempt_kind.as_deref(), Some("initial_upload"))
        && document_context.document.current_revision_id.is_none()
    {
        if let Some(target_revision_id) = document_context.target_revision_id {
            repositories::activate_document_revision(
                &state.persistence.postgres,
                document_context.document.id,
                target_revision_id,
            )
            .await
            .with_context(|| {
                format!("failed to activate initial revision {}", target_revision_id)
            })?;
        }
    }
    repositories::update_document_current_revision(
        &state.persistence.postgres,
        document_context.document.id,
        document_context.target_revision_id.or(document_context.document.current_revision_id),
        terminal_status,
        None,
        None,
    )
    .await
    .with_context(|| {
        format!("failed to finalize logical document {}", document_context.document.id)
    })?;
    if let Some(mutation_workflow_id) = payload.document_mutation_workflow_id {
        repositories::complete_document_mutation_impact_scope(
            &state.persistence.postgres,
            mutation_workflow_id,
            "completed",
            None,
        )
        .await
        .with_context(|| {
            format!(
                "failed to mark document mutation impact scope {mutation_workflow_id} as completed"
            )
        })?;
        repositories::update_document_mutation_workflow_status(
            &state.persistence.postgres,
            mutation_workflow_id,
            "completed",
            None,
        )
        .await
        .with_context(|| {
            format!("failed to mark document mutation workflow {mutation_workflow_id} as completed")
        })?;
    }
    Ok(())
}

async fn finalize_document_attempt_failure(
    state: &AppState,
    payload: &repositories::IngestionExecutionPayload,
    error_message: &str,
) -> anyhow::Result<()> {
    if let Some(target_revision_id) = payload.target_revision_id {
        repositories::update_document_revision_status(
            &state.persistence.postgres,
            target_revision_id,
            "failed",
        )
        .await
        .with_context(|| format!("failed to mark revision {target_revision_id} as failed"))?;

        if let Some(current_revision) =
            state.arango_document_store.get_revision(target_revision_id).await.with_context(
                || format!("failed to load knowledge revision {target_revision_id}"),
            )?
        {
            let preserved = preserve_failure_revision_truth(&current_revision);
            let _ = state
                .arango_document_store
                .update_revision_readiness(
                    target_revision_id,
                    &preserved.text_state,
                    &preserved.vector_state,
                    &preserved.graph_state,
                    preserved.text_readable_at,
                    preserved.vector_ready_at,
                    preserved.graph_ready_at,
                    current_revision.superseded_by_revision_id,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to preserve knowledge revision readiness {} after terminal failure",
                        target_revision_id
                    )
                })?;

            if let Some(project) =
                repositories::get_project_by_id(&state.persistence.postgres, payload.project_id)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load project {} while preserving failed knowledge truth",
                            payload.project_id
                        )
                    })?
            {
                let existing_generation = state
                    .arango_document_store
                    .list_library_generations(payload.project_id)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to list knowledge generations for library {} after terminal failure",
                            payload.project_id
                        )
                    })?
                    .into_iter()
                    .next();
                if let Some(snapshot) = build_failure_generation_snapshot(
                    existing_generation.as_ref(),
                    &current_revision,
                    &preserved,
                ) {
                    state
                        .canonical_services
                        .knowledge
                        .refresh_library_generation(
                            state,
                            RefreshKnowledgeLibraryGenerationCommand {
                                generation_id: snapshot.generation_id,
                                workspace_id: project.workspace_id,
                                library_id: payload.project_id,
                                active_text_generation: snapshot.active_text_generation,
                                active_vector_generation: snapshot.active_vector_generation,
                                active_graph_generation: snapshot.active_graph_generation,
                                degraded_state: snapshot.degraded_state,
                            },
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "failed to refresh knowledge generation for library {} after terminal failure",
                                payload.project_id
                            )
                        })?;
                }
            }
        }
    }
    if let Some(mutation_workflow_id) = payload.document_mutation_workflow_id {
        let _ = repositories::complete_document_mutation_impact_scope(
            &state.persistence.postgres,
            mutation_workflow_id,
            "failed",
            Some(error_message),
        )
        .await;
        repositories::update_document_mutation_workflow_status(
            &state.persistence.postgres,
            mutation_workflow_id,
            "failed",
            Some(error_message),
        )
        .await
        .with_context(|| {
            format!("failed to mark document mutation workflow {mutation_workflow_id} as failed")
        })?;
    }
    if let Some(document_id) = payload.logical_document_id {
        if let Some(document) =
            repositories::get_document_by_id(&state.persistence.postgres, document_id).await?
        {
            let fallback_status =
                if document.current_revision_id.is_some() && document.deleted_at.is_none() {
                    "ready"
                } else {
                    "failed"
                };
            repositories::update_document_current_revision(
                &state.persistence.postgres,
                document_id,
                document.current_revision_id,
                fallback_status,
                payload.mutation_kind.as_deref(),
                payload.mutation_kind.as_deref().map(|_| "failed"),
            )
            .await
            .with_context(|| {
                format!("failed to restore logical document {document_id} after mutation failure")
            })?;
        }
    }
    Ok(())
}

fn is_revision_update_mutation(payload: &repositories::IngestionExecutionPayload) -> bool {
    matches!(payload.mutation_kind.as_deref(), Some("update_append" | "update_replace"))
}

pub async fn fail_job(
    state: &AppState,
    job_id: Uuid,
    attempt_no: Option<i32>,
    runtime_ingestion_run_id: Option<Uuid>,
    worker_id: &str,
    elapsed_ms: u128,
    error: &anyhow::Error,
) {
    let message = error.to_string();
    error!(
        job_id = %job_id,
        %worker_id,
        attempt_no,
        elapsed_ms,
        error = %message,
        error_debug = ?error,
        "ingestion job failed",
    );

    if let Some(attempt_no) = attempt_no {
        if let Err(attempt_error) = repositories::fail_ingestion_job_attempt(
            &state.persistence.postgres,
            job_id,
            attempt_no,
            worker_id,
            "failed",
            &message,
        )
        .await
        {
            error!(job_id=%job_id, %worker_id, ?attempt_error, original_error=%message, "failed to mark ingestion job attempt as failed");
        }
    }

    if let Err(finalize_error) =
        repositories::fail_ingestion_job(&state.persistence.postgres, job_id, worker_id, &message)
            .await
    {
        error!(job_id=%job_id, %worker_id, ?finalize_error, original_error=%message, "failed to mark ingestion job as failed");
    }

    let runtime_stage_snapshot = if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        match repositories::get_runtime_ingestion_run_by_id(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
        )
        .await
        {
            Ok(Some(run)) => Some(run),
            Ok(None) => None,
            Err(load_error) => {
                error!(
                    job_id = %job_id,
                    %worker_id,
                    runtime_ingestion_run_id = %runtime_ingestion_run_id,
                    ?load_error,
                    "failed to load runtime ingestion run before failure reconciliation"
                );
                None
            }
        }
    } else {
        None
    };

    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        if let Err(runtime_error) = repositories::update_runtime_ingestion_run_status(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
            "failed",
            "failed",
            None,
            Some(&message),
        )
        .await
        {
            error!(
                job_id = %job_id,
                %worker_id,
                runtime_ingestion_run_id = %runtime_ingestion_run_id,
                ?runtime_error,
                "failed to mark runtime ingestion run as failed"
            );
        }
        if let (Some(attempt_no), Some(runtime_stage_snapshot)) =
            (attempt_no, runtime_stage_snapshot.as_ref())
        {
            if let Err(runtime_stage_error) = append_failed_runtime_stage_sequence(
                state,
                runtime_ingestion_run_id,
                attempt_no,
                &runtime_stage_snapshot.current_stage,
                &message,
                job_id,
            )
            .await
            {
                error!(
                    job_id = %job_id,
                    %worker_id,
                    runtime_ingestion_run_id = %runtime_ingestion_run_id,
                    ?runtime_stage_error,
                    "failed to append runtime failure benchmark sequence"
                );
            }
        }
    }
    match repositories::get_ingestion_job_by_id(&state.persistence.postgres, job_id).await {
        Ok(Some(job)) => match repositories::parse_ingestion_execution_payload(&job) {
            Ok(payload) => {
                if let Err(document_error) =
                    finalize_document_attempt_failure(state, &payload, &message).await
                {
                    error!(
                        job_id = %job_id,
                        %worker_id,
                        ?document_error,
                        "failed to finalize document lifecycle after ingestion failure"
                    );
                }
            }
            Err(payload_error) => {
                error!(
                    job_id = %job_id,
                    %worker_id,
                    ?payload_error,
                    "failed to parse ingestion payload while finalizing document lifecycle failure"
                );
            }
        },
        Ok(None) => {}
        Err(load_error) => {
            error!(
                job_id = %job_id,
                %worker_id,
                ?load_error,
                "failed to load ingestion job while finalizing document lifecycle failure"
            );
        }
    }
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn is_rebuild_follow_up_job(job: &IngestionJobRow, graph_status: Option<&str>) -> bool {
    let trigger_kind = job.trigger_kind.to_ascii_lowercase();
    trigger_kind.contains("reprocess") || matches!(graph_status, Some("stale" | "building"))
}

fn extracting_content_stage_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "preparing extracted content while graph coverage is being refreshed"
    } else {
        "persisting extracted content"
    }
}

fn chunking_stage_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "re-splitting extracted content for a graph rebuild follow-up run"
    } else {
        "splitting extracted content into chunks"
    }
}

fn chunking_completed_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "chunking completed for the rebuild follow-up run"
    } else {
        "chunking completed"
    }
}

fn embedding_chunks_stage_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "re-embedding chunks before refreshing graph coverage"
    } else {
        "embedding chunks for retrieval"
    }
}

fn embedding_chunks_completed_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "chunk embeddings refreshed for the rebuild follow-up run"
    } else {
        "chunk embeddings persisted"
    }
}

fn extracting_graph_stage_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "extracting entities and relations while stale graph coverage is being refreshed"
    } else {
        "extracting entities and relations from chunks"
    }
}

fn extracting_graph_completed_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "graph extraction completed for the rebuild follow-up run"
    } else {
        "graph extraction completed"
    }
}

fn merging_graph_stage_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "merging extracted graph knowledge into the refreshed library graph"
    } else {
        "merging extracted graph knowledge"
    }
}

fn merging_graph_completed_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "canonical graph merge completed for the rebuild follow-up run"
    } else {
        "canonical graph merge completed"
    }
}

fn projecting_graph_stage_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "refreshing the canonical graph view after a delete or reprocess mutation"
    } else {
        "refreshing the canonical graph view"
    }
}

fn projecting_graph_completed_message(rebuild_follow_up: bool, graph_status: &str) -> &'static str {
    match (rebuild_follow_up, graph_status) {
        (_, "ready") if rebuild_follow_up => "stale graph view refreshed",
        (_, "ready") => "canonical graph view refreshed",
        (true, _) => {
            "projection skipped because the rebuild follow-up run produced no graph evidence"
        }
        (false, _) => "projection skipped because no graph evidence was produced",
    }
}

fn worker_lease_duration(settings: &Settings) -> chrono::Duration {
    let seconds =
        settings.ingestion_worker_lease_seconds.max(DEFAULT_WORKER_LEASE_DURATION.as_secs());
    chrono::Duration::seconds(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn worker_heartbeat_interval(settings: &Settings) -> Duration {
    Duration::from_secs(
        settings
            .ingestion_worker_heartbeat_interval_seconds
            .max(DEFAULT_WORKER_HEARTBEAT_INTERVAL.as_secs()),
    )
}

fn worker_stale_heartbeat_grace(settings: &Settings) -> chrono::Duration {
    let heartbeat_secs = i64::try_from(
        settings
            .ingestion_worker_heartbeat_interval_seconds
            .max(DEFAULT_WORKER_HEARTBEAT_INTERVAL.as_secs()),
    )
    .unwrap_or(i64::MAX / 3);
    let llm_timeout_secs =
        i64::try_from(settings.llm_http_timeout_seconds.max(1)).unwrap_or(i64::MAX / 3);
    chrono::Duration::seconds(
        (heartbeat_secs * 3)
            .max(llm_timeout_secs.saturating_add(heartbeat_secs))
            .max(DEFAULT_STALE_WORKER_GRACE_SECONDS),
    )
}

fn finalizing_stage_message(rebuild_follow_up: bool) -> &'static str {
    if rebuild_follow_up {
        "finalizing runtime ingestion after a graph rebuild follow-up"
    } else {
        "finalizing runtime ingestion"
    }
}

fn finalizing_completed_message(rebuild_follow_up: bool, terminal_status: &str) -> &'static str {
    match (rebuild_follow_up, terminal_status) {
        (true, "ready") => "document finished and stale graph coverage has been refreshed",
        (true, _) => "document finished but the rebuild follow-up run produced no graph evidence",
        (false, "ready") => "document and graph are ready",
        (false, _) => "document is ready but no graph evidence exists yet",
    }
}

fn graph_stage_progress_percent(processed_chunks: usize, total_chunks: usize) -> Option<i32> {
    if processed_chunks == 0 || total_chunks == 0 {
        return None;
    }

    let spread = EXTRACTING_GRAPH_PROGRESS_END_PERCENT - EXTRACTING_GRAPH_PROGRESS_START_PERCENT;
    let ratio = processed_chunks as f64 / total_chunks as f64;
    let progress =
        EXTRACTING_GRAPH_PROGRESS_START_PERCENT + (ratio * f64::from(spread)).ceil() as i32;

    Some(
        progress.clamp(
            EXTRACTING_GRAPH_PROGRESS_START_PERCENT + 1,
            EXTRACTING_GRAPH_PROGRESS_END_PERCENT,
        ),
    )
}

fn should_persist_graph_progress_checkpoint(
    tracker: &GraphStageProgressTracker,
    next_progress: i32,
    checkpoint_interval: Duration,
) -> bool {
    next_progress > tracker.last_persisted_progress
        || tracker.last_persisted_at.elapsed() >= checkpoint_interval
}

async fn maybe_persist_graph_progress_checkpoint(
    state: &AppState,
    runtime_ingestion_run_id: Option<Uuid>,
    attempt_no: i32,
    tracker: &mut GraphStageProgressTracker,
    processed_chunks: usize,
    total_chunks: usize,
) -> anyhow::Result<()> {
    let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id else {
        return Ok(());
    };
    let Some(next_progress) = graph_stage_progress_percent(processed_chunks, total_chunks) else {
        return Ok(());
    };
    let checkpoint_interval = Duration::from_secs(
        state.pipeline_hardening.graph_progress_checkpoint_interval_seconds.max(1),
    );
    if !should_persist_graph_progress_checkpoint(tracker, next_progress, checkpoint_interval) {
        return Ok(());
    }

    let persisted_at = Utc::now();
    repositories::update_runtime_ingestion_run_processing_stage_checkpoint(
        &state.persistence.postgres,
        runtime_ingestion_run_id,
        "extracting_graph",
        next_progress,
        persisted_at,
    )
    .await?;
    repositories::upsert_runtime_graph_progress_checkpoint(
        &state.persistence.postgres,
        &repositories::RuntimeGraphProgressCheckpointInput {
            ingestion_run_id: runtime_ingestion_run_id,
            attempt_no,
            processed_chunks: i64::try_from(processed_chunks).unwrap_or(i64::MAX),
            total_chunks: i64::try_from(total_chunks).unwrap_or(i64::MAX),
            progress_percent: Some(next_progress),
            provider_call_count: i64::try_from(tracker.provider_call_count).unwrap_or(i64::MAX),
            avg_call_elapsed_ms: tracker.avg_call_elapsed_ms(),
            avg_chunk_elapsed_ms: tracker.avg_chunk_elapsed_ms(),
            avg_chars_per_second: tracker.avg_chars_per_second(),
            avg_tokens_per_second: tracker.avg_tokens_per_second(),
            last_provider_call_at: tracker.last_provider_call_at,
            next_checkpoint_eta_ms: tracker.next_checkpoint_eta_ms(total_chunks),
            pressure_kind: graph_progress_pressure_kind(tracker, total_chunks).map(str::to_string),
            computed_at: persisted_at,
        },
    )
    .await?;
    tracker.last_persisted_progress = tracker.last_persisted_progress.max(next_progress);
    tracker.last_persisted_at = Instant::now();
    Ok(())
}

fn graph_progress_pressure_kind(
    tracker: &GraphStageProgressTracker,
    total_chunks: usize,
) -> Option<&'static str> {
    let remaining_chunks = total_chunks.saturating_sub(tracker.processed_chunks);
    match (remaining_chunks, tracker.avg_chunk_elapsed_ms()) {
        (0, _) => Some("steady"),
        (_, Some(avg_chunk_elapsed_ms)) if avg_chunk_elapsed_ms >= 10_000 => Some("high"),
        (_, Some(avg_chunk_elapsed_ms)) if avg_chunk_elapsed_ms >= 4_000 => Some("elevated"),
        (_, Some(_)) => Some("steady"),
        _ => None,
    }
}

fn collect_graph_embedding_support_node_ids(
    changed_node_ids: &BTreeSet<Uuid>,
    changed_edges: &[repositories::RuntimeGraphEdgeRow],
) -> Vec<Uuid> {
    let mut node_ids = changed_node_ids.clone();
    for edge in changed_edges {
        node_ids.insert(edge.from_node_id);
        node_ids.insert(edge.to_node_id);
    }
    node_ids.into_iter().collect()
}

async fn start_runtime_stage(
    state: &AppState,
    runtime_ingestion_run_id: Option<Uuid>,
    attempt_no: i32,
    stage_name: &str,
    progress_percent: Option<i32>,
    message: Option<&str>,
    ingestion_job_id: Uuid,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
) -> anyhow::Result<Option<RuntimeStageSpan>> {
    let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id else {
        return Ok(None);
    };
    let stage_started_at = Utc::now();

    repositories::update_runtime_ingestion_run_processing_stage(
        &state.persistence.postgres,
        runtime_ingestion_run_id,
        stage_name,
        progress_percent,
        stage_started_at,
        None,
    )
    .await?;
    let stage_event = repositories::append_runtime_stage_event(
        &state.persistence.postgres,
        runtime_ingestion_run_id,
        attempt_no,
        stage_name,
        "started",
        message,
        stage_event_metadata(
            ingestion_job_id,
            provider_kind,
            model_name,
            stage_started_at,
            None,
            None,
        ),
    )
    .await?;
    Ok(Some(RuntimeStageSpan {
        stage_event_id: stage_event.id,
        stage: stage_name.to_string(),
        started_at: stage_started_at,
        provider_kind: provider_kind.map(str::to_string),
        model_name: model_name.map(str::to_string),
    }))
}

async fn complete_runtime_stage(
    state: &AppState,
    runtime_ingestion_run_id: Uuid,
    attempt_no: i32,
    stage_span: &RuntimeStageSpan,
    message: Option<&str>,
    ingestion_job_id: Uuid,
) -> anyhow::Result<repositories::RuntimeIngestionStageEventRow> {
    complete_runtime_stage_with_status(
        state,
        runtime_ingestion_run_id,
        attempt_no,
        stage_span,
        "completed",
        message,
        ingestion_job_id,
    )
    .await
}

async fn complete_runtime_stage_with_status(
    state: &AppState,
    runtime_ingestion_run_id: Uuid,
    attempt_no: i32,
    stage_span: &RuntimeStageSpan,
    status: &str,
    message: Option<&str>,
    ingestion_job_id: Uuid,
) -> anyhow::Result<repositories::RuntimeIngestionStageEventRow> {
    let finished_at = Utc::now();
    repositories::update_runtime_ingestion_run_activity(
        &state.persistence.postgres,
        runtime_ingestion_run_id,
        if status == "failed" { "failed" } else { "active" },
        finished_at,
        None,
    )
    .await?;
    repositories::append_runtime_stage_event(
        &state.persistence.postgres,
        runtime_ingestion_run_id,
        attempt_no,
        &stage_span.stage,
        status,
        message,
        stage_event_metadata(
            ingestion_job_id,
            stage_span.provider_kind.as_deref(),
            stage_span.model_name.as_deref(),
            stage_span.started_at,
            Some(finished_at),
            Some(
                finished_at.signed_duration_since(stage_span.started_at).num_milliseconds().max(0),
            ),
        ),
    )
    .await
    .map_err(Into::into)
}

async fn maybe_record_extraction_stage_accounting(
    state: &AppState,
    workspace_id: Option<Uuid>,
    project_id: Uuid,
    runtime_ingestion_run_id: Uuid,
    stage_name: &str,
    stage_event: &repositories::RuntimeIngestionStageEventRow,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
) -> anyhow::Result<()> {
    let (Some(provider_kind), Some(model_name)) = (provider_kind, model_name) else {
        return Ok(());
    };
    let _ = document_accounting::record_stage_accounting_gap(
        state,
        document_accounting::StageAccountingGapRequest {
            ingestion_run_id: runtime_ingestion_run_id,
            stage_event_id: stage_event.id,
            stage: stage_name.to_string(),
            accounting_scope: document_accounting::StageAccountingScope::StageRollup,
            workspace_id,
            project_id: Some(project_id),
            provider_kind: Some(provider_kind.to_string()),
            model_name: Some(model_name.to_string()),
            capability: PricingCapability::Vision,
            billing_unit: PricingBillingUnit::Per1MTokens,
            pricing_status: PricingResolutionStatus::UsageMissing,
            token_usage_json: serde_json::json!({
                "call_count": 1,
                "usage_missing": true,
            }),
            pricing_snapshot_json: serde_json::json!({
                "status": "usage_missing",
                "provider_kind": provider_kind,
                "model_name": model_name,
                "capability": "vision",
                "billing_unit": "per_1m_tokens",
            }),
        },
    )
    .await?;
    Ok(())
}

async fn maybe_record_usage_stage_accounting(
    state: &AppState,
    workspace_id: Option<Uuid>,
    project_id: Uuid,
    runtime_ingestion_run_id: Uuid,
    stage_name: &str,
    stage_event: &repositories::RuntimeIngestionStageEventRow,
    capability: PricingCapability,
    billing_unit: PricingBillingUnit,
    usage_kind: &str,
    model_profile_id: Option<Uuid>,
    usage: &RuntimeStageUsageSummary,
) -> anyhow::Result<()> {
    let (Some(provider_kind), Some(model_name)) =
        (usage.provider_kind.as_deref(), usage.model_name.as_deref())
    else {
        return Ok(());
    };
    if usage.call_count == 0 {
        return Ok(());
    }
    if !usage.has_token_usage() {
        let _ = document_accounting::record_stage_accounting_gap(
            state,
            document_accounting::StageAccountingGapRequest {
                ingestion_run_id: runtime_ingestion_run_id,
                stage_event_id: stage_event.id,
                stage: stage_name.to_string(),
                accounting_scope: document_accounting::StageAccountingScope::StageRollup,
                workspace_id,
                project_id: Some(project_id),
                provider_kind: Some(provider_kind.to_string()),
                model_name: Some(model_name.to_string()),
                capability,
                billing_unit,
                pricing_status: PricingResolutionStatus::UsageMissing,
                token_usage_json: usage.clone().into_usage_json(),
                pricing_snapshot_json: serde_json::json!({
                    "status": "usage_missing",
                    "provider_kind": provider_kind,
                    "model_name": model_name,
                }),
            },
        )
        .await?;
        return Ok(());
    }

    let _ = document_accounting::record_stage_usage_and_cost(
        state,
        document_accounting::StageUsageAccountingRequest {
            ingestion_run_id: runtime_ingestion_run_id,
            stage_event_id: stage_event.id,
            stage: stage_name.to_string(),
            accounting_scope: document_accounting::StageAccountingScope::StageRollup,
            workspace_id,
            project_id: Some(project_id),
            model_profile_id,
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            capability,
            billing_unit,
            usage_kind: usage_kind.to_string(),
            prompt_tokens: usage.prompt_tokens(),
            completion_tokens: usage.completion_tokens(),
            total_tokens: usage.total_tokens(),
            raw_usage_json: usage.clone().into_usage_json(),
        },
    )
    .await?;
    Ok(())
}

async fn persist_graph_extraction_recovery_attempts(
    state: &AppState,
    workspace_id: Option<Uuid>,
    project_id: Uuid,
    document: &repositories::DocumentRow,
    runtime_ingestion_run_id: Option<Uuid>,
    attempt_no: i32,
    chunk_id: Uuid,
    revision_id: Option<Uuid>,
    recovery_attempts: &[GraphExtractionRecoveryRecord],
) -> anyhow::Result<()> {
    let Some(workspace_id) = workspace_id else {
        return Ok(());
    };
    for attempt in recovery_attempts {
        let created = repositories::create_runtime_graph_extraction_recovery_attempt(
            &state.persistence.postgres,
            &repositories::CreateRuntimeGraphExtractionRecoveryAttemptInput {
                workspace_id,
                project_id,
                document_id: document.id,
                revision_id,
                ingestion_run_id: runtime_ingestion_run_id,
                attempt_no,
                chunk_id: Some(chunk_id),
                recovery_kind: attempt.recovery_kind.clone(),
                trigger_reason: attempt.trigger_reason.clone(),
                status: "started".to_string(),
                raw_issue_summary: attempt.raw_issue_summary.clone(),
                recovered_summary: None,
            },
        )
        .await?;
        let _ = repositories::update_runtime_graph_extraction_recovery_attempt_status(
            &state.persistence.postgres,
            created.id,
            &attempt.status,
            attempt.recovered_summary.as_deref(),
        )
        .await?;
    }
    Ok(())
}

fn stage_event_metadata(
    ingestion_job_id: Uuid,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    elapsed_ms: Option<i64>,
) -> serde_json::Value {
    serde_json::json!({
        "ingestion_job_id": ingestion_job_id,
        "provider_kind": provider_kind,
        "model_name": model_name,
        "started_at": started_at,
        "finished_at": finished_at,
        "elapsed_ms": elapsed_ms,
    })
}

async fn append_failed_runtime_stage_sequence(
    state: &AppState,
    runtime_ingestion_run_id: Uuid,
    attempt_no: i32,
    current_stage: &str,
    error_message: &str,
    ingestion_job_id: Uuid,
) -> anyhow::Result<()> {
    let active_span =
        latest_runtime_stage_span(state, runtime_ingestion_run_id, attempt_no, current_stage)
            .await?;
    let failed_span = active_span.unwrap_or_else(|| RuntimeStageSpan {
        stage_event_id: Uuid::nil(),
        stage: current_stage.to_string(),
        started_at: Utc::now(),
        provider_kind: None,
        model_name: None,
    });
    let failed_event = complete_runtime_stage_with_status(
        state,
        runtime_ingestion_run_id,
        attempt_no,
        &failed_span,
        "failed",
        Some(error_message),
        ingestion_job_id,
    )
    .await?;
    let failed_at = failed_event.finished_at.unwrap_or(failed_event.started_at);
    let mut mark_skipped = false;
    for stage in RUNTIME_STAGE_SEQUENCE {
        if stage == current_stage {
            mark_skipped = true;
            continue;
        }
        if !mark_skipped {
            continue;
        }
        let skipped_span = RuntimeStageSpan {
            stage_event_id: Uuid::nil(),
            stage: stage.to_string(),
            started_at: failed_at,
            provider_kind: None,
            model_name: None,
        };
        complete_runtime_stage_with_status(
            state,
            runtime_ingestion_run_id,
            attempt_no,
            &skipped_span,
            "skipped",
            Some("skipped after an earlier stage failed"),
            ingestion_job_id,
        )
        .await?;
    }
    Ok(())
}

async fn latest_runtime_stage_span(
    state: &AppState,
    runtime_ingestion_run_id: Uuid,
    attempt_no: i32,
    stage_name: &str,
) -> anyhow::Result<Option<RuntimeStageSpan>> {
    let events = repositories::list_runtime_stage_events_by_run(
        &state.persistence.postgres,
        runtime_ingestion_run_id,
    )
    .await?;
    Ok(events
        .into_iter()
        .rev()
        .find(|event| {
            event.attempt_no == attempt_no && event.stage == stage_name && event.status == "started"
        })
        .map(|event| RuntimeStageSpan {
            stage_event_id: event.id,
            stage: event.stage,
            started_at: event.started_at,
            provider_kind: event.provider_kind,
            model_name: event.model_name,
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{config::Settings, state::AppState},
        domains::ingest::IngestStageEvent,
        infra::arangodb::document_store::{KnowledgeLibraryGenerationRow, KnowledgeRevisionRow},
        infra::repositories::{self, IngestionJobRow},
        services::content_service::derive_failed_revision_readiness,
    };

    fn sample_job(trigger_kind: &str) -> IngestionJobRow {
        IngestionJobRow {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            source_id: None,
            trigger_kind: trigger_kind.to_string(),
            status: "queued".to_string(),
            stage: "accepted".to_string(),
            requested_by: None,
            error_message: None,
            started_at: None,
            finished_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            idempotency_key: None,
            parent_job_id: None,
            attempt_count: 0,
            worker_id: None,
            lease_expires_at: None,
            heartbeat_at: None,
            payload_json: serde_json::json!({}),
            result_json: serde_json::json!({}),
        }
    }

    fn sample_revision() -> KnowledgeRevisionRow {
        KnowledgeRevisionRow {
            key: "revision".to_string(),
            arango_id: None,
            arango_rev: None,
            revision_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "file".to_string(),
            storage_ref: None,
            source_uri: None,
            mime_type: "application/pdf".to_string(),
            checksum: "checksum".to_string(),
            title: Some("sample.pdf".to_string()),
            byte_size: 1024,
            normalized_text: Some("sample".to_string()),
            text_checksum: Some("checksum".to_string()),
            text_state: "accepted".to_string(),
            vector_state: "accepted".to_string(),
            graph_state: "accepted".to_string(),
            text_readable_at: None,
            vector_ready_at: None,
            graph_ready_at: None,
            superseded_by_revision_id: None,
            created_at: chrono::Utc::now(),
        }
    }

    fn completed_stage(stage_name: &str) -> IngestStageEvent {
        IngestStageEvent {
            id: Uuid::now_v7(),
            attempt_id: Uuid::now_v7(),
            stage_name: stage_name.to_string(),
            stage_state: "completed".to_string(),
            ordinal: 1,
            message: None,
            details_json: serde_json::json!({}),
            recorded_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn treats_reprocess_trigger_as_rebuild_follow_up() {
        assert!(is_rebuild_follow_up_job(&sample_job("ui_reprocess"), Some("ready")));
        assert!(is_rebuild_follow_up_job(&sample_job("runtime_upload"), Some("stale")));
        assert!(!is_rebuild_follow_up_job(&sample_job("runtime_upload"), Some("ready")));
    }

    #[test]
    fn uses_follow_up_finalizing_copy_for_reprocess_runs() {
        assert_eq!(
            finalizing_completed_message(true, "ready"),
            "document finished and stale graph coverage has been refreshed"
        );
        assert_eq!(
            projecting_graph_completed_message(true, "empty"),
            "projection skipped because the rebuild follow-up run produced no graph evidence"
        );
    }

    #[test]
    fn worker_ids_use_service_identity_namespace() {
        let worker_id = ingestion_worker_id("rustrag-worker", 2);

        assert!(worker_id.starts_with("rustrag-worker:2:"));
    }

    #[test]
    fn lease_recovery_ids_use_service_identity_namespace() {
        assert_eq!(lease_recovery_worker_id("rustrag-worker"), "rustrag-worker:lease-recovery");
    }

    #[test]
    fn graph_stage_progress_advances_with_chunk_completion() {
        assert_eq!(graph_stage_progress_percent(0, 10), None);
        assert_eq!(graph_stage_progress_percent(1, 10), Some(83));
        assert_eq!(graph_stage_progress_percent(5, 10), Some(85));
        assert_eq!(graph_stage_progress_percent(10, 10), Some(87));
    }

    #[test]
    fn graph_progress_checkpoint_persists_on_progress_or_stale_activity() {
        let tracker = GraphStageProgressTracker {
            last_persisted_progress: EXTRACTING_GRAPH_PROGRESS_START_PERCENT,
            last_persisted_at: Instant::now(),
            processed_chunks: 0,
            provider_call_count: 0,
            total_call_elapsed_ms: 0,
            chars_per_second_sum: 0.0,
            chars_per_second_samples: 0,
            tokens_per_second_sum: 0.0,
            tokens_per_second_samples: 0,
            last_provider_call_at: None,
        };
        assert!(should_persist_graph_progress_checkpoint(
            &tracker,
            83,
            GRAPH_PROGRESS_ACTIVITY_INTERVAL,
        ));
        assert!(!should_persist_graph_progress_checkpoint(
            &tracker,
            82,
            GRAPH_PROGRESS_ACTIVITY_INTERVAL,
        ));

        let stale_tracker = GraphStageProgressTracker {
            last_persisted_progress: EXTRACTING_GRAPH_PROGRESS_END_PERCENT,
            last_persisted_at: Instant::now() - GRAPH_PROGRESS_ACTIVITY_INTERVAL,
            processed_chunks: 0,
            provider_call_count: 0,
            total_call_elapsed_ms: 0,
            chars_per_second_sum: 0.0,
            chars_per_second_samples: 0,
            tokens_per_second_sum: 0.0,
            tokens_per_second_samples: 0,
            last_provider_call_at: None,
        };
        assert!(should_persist_graph_progress_checkpoint(
            &stale_tracker,
            EXTRACTING_GRAPH_PROGRESS_END_PERCENT,
            GRAPH_PROGRESS_ACTIVITY_INTERVAL,
        ));
    }

    #[test]
    fn failed_readiness_marks_unfinished_stages_as_failed() {
        let revision = sample_revision();

        let readiness = derive_failed_revision_readiness(&revision, &[]);

        assert_eq!(readiness.text_state, "failed");
        assert_eq!(readiness.vector_state, "failed");
        assert_eq!(readiness.graph_state, "failed");
    }

    #[test]
    fn failed_readiness_preserves_completed_stage_progress() {
        let revision = sample_revision();
        let stage_events = vec![completed_stage("extract_content"), completed_stage("embed_chunk")];

        let readiness = derive_failed_revision_readiness(&revision, &stage_events);

        assert_eq!(readiness.text_state, "text_readable");
        assert_eq!(readiness.vector_state, "ready");
        assert_eq!(readiness.graph_state, "failed");
        assert!(readiness.text_readable_at.is_some());
        assert!(readiness.vector_ready_at.is_some());
        assert_eq!(readiness.graph_ready_at, None);
    }

    #[test]
    fn terminal_failure_preserves_reached_readiness_states() {
        let now = Utc::now();
        let revision = KnowledgeRevisionRow {
            key: "revision-1".to_string(),
            arango_id: None,
            arango_rev: None,
            revision_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_number: 7,
            revision_state: "processing".to_string(),
            revision_kind: "replace".to_string(),
            storage_ref: None,
            source_uri: None,
            mime_type: "text/plain".to_string(),
            checksum: "abc".to_string(),
            title: Some("Document".to_string()),
            byte_size: 128,
            normalized_text: Some("hello".to_string()),
            text_checksum: Some("def".to_string()),
            text_state: "ready".to_string(),
            vector_state: "ready".to_string(),
            graph_state: "processing".to_string(),
            text_readable_at: Some(now),
            vector_ready_at: Some(now),
            graph_ready_at: None,
            superseded_by_revision_id: None,
            created_at: now,
        };

        let preserved = preserve_failure_revision_truth(&revision);

        assert_eq!(preserved.text_state, "ready");
        assert_eq!(preserved.vector_state, "ready");
        assert_eq!(preserved.graph_state, "failed");
        assert_eq!(preserved.text_readable_at, Some(now));
        assert_eq!(preserved.vector_ready_at, Some(now));
        assert_eq!(preserved.graph_ready_at, None);

        let existing_generation = KnowledgeLibraryGenerationRow {
            key: "generation-1".to_string(),
            arango_id: None,
            arango_rev: None,
            generation_id: Uuid::now_v7(),
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            active_text_generation: 3,
            active_vector_generation: 4,
            active_graph_generation: 0,
            degraded_state: "degraded".to_string(),
            updated_at: now,
        };

        let snapshot =
            build_failure_generation_snapshot(Some(&existing_generation), &revision, &preserved)
                .expect("snapshot should be produced when readability was reached");

        assert_eq!(snapshot.active_text_generation, 7);
        assert_eq!(snapshot.active_vector_generation, 7);
        assert_eq!(snapshot.active_graph_generation, 0);
        assert_eq!(snapshot.degraded_state, "degraded");
    }

    #[test]
    fn terminal_failure_marks_unreached_readiness_failed() {
        let now = Utc::now();
        let revision = KnowledgeRevisionRow {
            key: "revision-2".to_string(),
            arango_id: None,
            arango_rev: None,
            revision_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_number: 4,
            revision_state: "processing".to_string(),
            revision_kind: "upload".to_string(),
            storage_ref: None,
            source_uri: None,
            mime_type: "text/plain".to_string(),
            checksum: "ghi".to_string(),
            title: None,
            byte_size: 64,
            normalized_text: None,
            text_checksum: None,
            text_state: "processing".to_string(),
            vector_state: "accepted".to_string(),
            graph_state: "processing".to_string(),
            text_readable_at: None,
            vector_ready_at: None,
            graph_ready_at: None,
            superseded_by_revision_id: None,
            created_at: now,
        };

        let preserved = preserve_failure_revision_truth(&revision);

        assert_eq!(preserved.text_state, "failed");
        assert_eq!(preserved.vector_state, "failed");
        assert_eq!(preserved.graph_state, "failed");
        assert_eq!(preserved.text_readable_at, None);
        assert_eq!(preserved.vector_ready_at, None);
        assert_eq!(preserved.graph_ready_at, None);
        assert!(
            build_failure_generation_snapshot(None, &revision, &preserved).is_none(),
            "no generation snapshot should be emitted when no readiness was reached"
        );
    }

    #[test]
    fn graph_edge_embedding_support_nodes_include_changed_edge_endpoints() {
        let changed_node_ids = BTreeSet::from([Uuid::now_v7()]);
        let source_node_id = Uuid::now_v7();
        let target_node_id = Uuid::now_v7();
        let support_node_ids = collect_graph_embedding_support_node_ids(
            &changed_node_ids,
            &[repositories::RuntimeGraphEdgeRow {
                id: Uuid::now_v7(),
                project_id: Uuid::now_v7(),
                from_node_id: source_node_id,
                to_node_id: target_node_id,
                relation_type: "mentions".to_string(),
                canonical_key: "document--mentions--entity".to_string(),
                summary: None,
                weight: None,
                support_count: 1,
                metadata_json: serde_json::json!({}),
                projection_version: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }],
        );

        assert!(support_node_ids.contains(&source_node_id));
        assert!(support_node_ids.contains(&target_node_id));
        assert!(support_node_ids.iter().any(|id| changed_node_ids.contains(id)));
    }

    #[test]
    fn cleanup_retry_artifacts_runs_only_for_non_mutation_replays_with_existing_state() {
        let payload = repositories::IngestionExecutionPayload {
            project_id: Uuid::now_v7(),
            runtime_ingestion_run_id: None,
            upload_batch_id: None,
            logical_document_id: None,
            target_revision_id: None,
            document_mutation_workflow_id: None,
            stale_guard_revision_no: None,
            attempt_kind: Some("initial_upload".to_string()),
            mutation_kind: None,
            source_id: None,
            external_key: "retry-fixture".to_string(),
            title: None,
            mime_type: Some("text/plain".to_string()),
            text: Some("retry fixture".to_string()),
            file_kind: Some("txt".to_string()),
            file_size_bytes: Some(32),
            adapter_status: None,
            extraction_error: None,
            extraction_kind: Some("text_like".to_string()),
            page_count: None,
            extraction_warnings: Vec::new(),
            source_map: serde_json::json!({}),
            extraction_provider_kind: None,
            extraction_model_name: None,
            extraction_version: None,
            ingest_mode: "runtime_upload".to_string(),
            extra_metadata: serde_json::json!({}),
        };
        let revision = repositories::DocumentRevisionRow {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_no: 1,
            revision_kind: "initial_upload".to_string(),
            parent_revision_id: None,
            source_file_name: "retry-fixture.txt".to_string(),
            appended_text_excerpt: None,
            accepted_at: Utc::now(),
            activated_at: Some(Utc::now()),
            superseded_at: None,
            content_hash: None,
            status: "ready".to_string(),
            mime_type: Some("text/plain".to_string()),
            file_size_bytes: Some(32),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(should_cleanup_retry_attempt_artifacts(&payload, Some(&revision), &[],));
        assert!(should_cleanup_retry_attempt_artifacts(&payload, None, &[Uuid::now_v7()],));

        let mut mutation_payload = payload.clone();
        mutation_payload.mutation_kind = Some("update_append".to_string());
        assert!(!should_cleanup_retry_attempt_artifacts(
            &mutation_payload,
            Some(&revision),
            &[Uuid::now_v7()],
        ));

        assert!(!should_cleanup_retry_attempt_artifacts(&payload, None, &[]));
    }

    #[tokio::test]
    #[ignore = "requires local postgres, redis, and arango services"]
    async fn cleanup_retry_artifacts_deletes_stale_chunks_for_replayed_initial_uploads() {
        let state =
            AppState::new(Settings::from_env().expect("settings")).await.expect("app state");
        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &format!("retry-clean-{}", Uuid::now_v7().simple()),
            "Retry Cleanup Workspace",
        )
        .await
        .expect("workspace");
        let project = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("retry-clean-lib-{}", Uuid::now_v7().simple()),
            "Retry Cleanup Library",
            Some("ingestion retry cleanup regression test"),
        )
        .await
        .expect("project");
        let document = repositories::create_document(
            &state.persistence.postgres,
            project.id,
            None,
            "retry-clean-fixture.txt",
            Some("Retry Cleanup Fixture"),
            Some("text/plain"),
            Some("deadbeef"),
        )
        .await
        .expect("document");
        let revision = repositories::create_document_revision(
            &state.persistence.postgres,
            document.id,
            1,
            "initial_upload",
            None,
            "retry-clean-fixture.txt",
            Some("text/plain"),
            Some(128),
            None,
            Some("deadbeef"),
        )
        .await
        .expect("revision");
        repositories::activate_document_revision(
            &state.persistence.postgres,
            document.id,
            revision.id,
        )
        .await
        .expect("activate revision");
        let document = repositories::update_document_current_revision(
            &state.persistence.postgres,
            document.id,
            Some(revision.id),
            "ready",
            None,
            None,
        )
        .await
        .expect("set current revision");
        let stale_chunks = vec![
            repositories::create_chunk(
                &state.persistence.postgres,
                document.id,
                project.id,
                0,
                "stale chunk 0",
                Some(3),
                serde_json::json!({}),
            )
            .await
            .expect("chunk 0"),
            repositories::create_chunk(
                &state.persistence.postgres,
                document.id,
                project.id,
                1,
                "stale chunk 1",
                Some(3),
                serde_json::json!({}),
            )
            .await
            .expect("chunk 1"),
        ];
        let stale_chunk_ids = stale_chunks.iter().map(|chunk| chunk.id).collect::<Vec<_>>();
        let payload = repositories::IngestionExecutionPayload {
            project_id: project.id,
            runtime_ingestion_run_id: None,
            upload_batch_id: None,
            logical_document_id: Some(document.id),
            target_revision_id: None,
            document_mutation_workflow_id: None,
            stale_guard_revision_no: Some(revision.revision_no),
            attempt_kind: Some("initial_upload".to_string()),
            mutation_kind: None,
            source_id: None,
            external_key: document.external_key.clone(),
            title: document.title.clone(),
            mime_type: document.mime_type.clone(),
            text: Some("fresh retry text".to_string()),
            file_kind: Some("txt".to_string()),
            file_size_bytes: Some(32),
            adapter_status: None,
            extraction_error: None,
            extraction_kind: Some("text_like".to_string()),
            page_count: None,
            extraction_warnings: Vec::new(),
            source_map: serde_json::json!({}),
            extraction_provider_kind: None,
            extraction_model_name: None,
            extraction_version: None,
            ingest_mode: "runtime_upload".to_string(),
            extra_metadata: serde_json::json!({}),
        };

        cleanup_retry_attempt_artifacts(
            &state,
            &payload,
            &document,
            Some(&revision),
            &stale_chunk_ids,
        )
        .await
        .expect("cleanup retry artifacts");

        let remaining_chunks =
            repositories::list_chunks_by_document(&state.persistence.postgres, document.id)
                .await
                .expect("remaining chunks");
        assert!(remaining_chunks.is_empty());
    }
}

async fn recover_expired_leases(state: &AppState, worker_id: &str) -> anyhow::Result<()> {
    let recovered_expired =
        repositories::recover_expired_ingestion_job_leases(&state.persistence.postgres).await?;
    handle_recovered_jobs(
        state,
        worker_id,
        recovered_expired,
        "lease_expired",
        "job lease expired before completion; requeued for retry",
        "requeued abandoned ingestion job after lease expiry",
        "recovered expired ingestion job leases",
    )
    .await?;

    let stale_before = Utc::now() - worker_stale_heartbeat_grace(&state.settings);
    let recovered_stale = repositories::recover_stale_ingestion_job_heartbeats(
        &state.persistence.postgres,
        stale_before,
    )
    .await?;
    handle_recovered_jobs(
        state,
        worker_id,
        recovered_stale,
        "worker_heartbeat_stalled",
        "worker heartbeat stalled before completion; requeued for retry",
        "requeued abandoned ingestion job after stale heartbeat",
        "recovered ingestion jobs abandoned by stale worker heartbeats",
    )
    .await?;

    let reconciled = repositories::reconcile_processing_runtime_ingestion_runs_with_queued_jobs(
        &state.persistence.postgres,
    )
    .await?;
    if !reconciled.is_empty() {
        let recovered_at = Utc::now();
        for run in &reconciled {
            let reason = match run.latest_error_message.as_deref() {
                Some(message) if !message.trim().is_empty() => message,
                _ => "worker heartbeat stalled before completion; requeued for retry",
            };
            repositories::update_runtime_ingestion_run_stage_activity(
                &state.persistence.postgres,
                run.id,
                "accepted",
                None,
                "retrying",
                recovered_at,
                Some(reason),
            )
            .await?;
        }
        warn!(
            %worker_id,
            reconciled_count = reconciled.len(),
            "reconciled runtime ingestion runs back to queued after stale processing state",
        );
    }

    let reconciled_failed =
        repositories::reconcile_processing_runtime_ingestion_runs_with_failed_jobs(
            &state.persistence.postgres,
        )
        .await?;
    if !reconciled_failed.is_empty() {
        let failed_at = Utc::now();
        for run in &reconciled_failed {
            let reason =
                run.latest_error_message.as_deref().unwrap_or("runtime ingestion attempt failed");
            repositories::update_runtime_ingestion_run_activity(
                &state.persistence.postgres,
                run.id,
                "failed",
                failed_at,
                None,
            )
            .await?;
            repositories::update_runtime_ingestion_run_stage_activity(
                &state.persistence.postgres,
                run.id,
                "failed",
                None,
                "failed",
                failed_at,
                Some(reason),
            )
            .await?;
        }
        warn!(
            %worker_id,
            reconciled_count = reconciled_failed.len(),
            "reconciled runtime ingestion runs to failed after terminal job errors",
        );
    }

    Ok(())
}

async fn handle_recovered_jobs(
    state: &AppState,
    worker_id: &str,
    recovered: Vec<repositories::RecoveredIngestionJobRow>,
    attempt_error_code: &str,
    runtime_stage_message: &str,
    per_job_log: &str,
    summary_log: &str,
) -> anyhow::Result<()> {
    let recovered_count = recovered.len();
    for job in recovered {
        let current_job = job.current_job();
        if let Ok(payload) = repositories::parse_ingestion_execution_payload(&current_job) {
            if let Some(runtime_ingestion_run_id) = payload.runtime_ingestion_run_id {
                match repositories::get_runtime_ingestion_run_by_id(
                    &state.persistence.postgres,
                    runtime_ingestion_run_id,
                )
                .await?
                {
                    Some(runtime_run) if runtime_run.status == "processing" => {
                        if let Err(runtime_stage_error) = append_failed_runtime_stage_sequence(
                            state,
                            runtime_ingestion_run_id,
                            job.attempt_count,
                            &runtime_run.current_stage,
                            runtime_stage_message,
                            job.id,
                        )
                        .await
                        {
                            warn!(
                                %worker_id,
                                job_id = %job.id,
                                runtime_ingestion_run_id = %runtime_ingestion_run_id,
                                ?runtime_stage_error,
                                "failed to append runtime stage failure during job recovery",
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        if job.attempt_count > 0 {
            repositories::fail_ingestion_job_attempt(
                &state.persistence.postgres,
                job.id,
                job.attempt_count,
                job.attempt_worker_id(worker_id),
                attempt_error_code,
                runtime_stage_message,
            )
            .await?;
        }
        warn!(
            %worker_id,
            job_id = %job.id,
            project_id = %job.project_id,
            source_id = ?job.source_id,
            previous_worker_id = ?job.previous_worker_id,
            attempt_no = job.attempt_count,
            previous_stage = %job.previous_stage,
            previous_status = %job.previous_status,
            current_stage = %job.stage,
            current_status = %job.status,
            recovery_reason = per_job_log,
            "requeued abandoned ingestion job during recovery",
        );
    }
    if recovered_count > 0 {
        warn!(
            %worker_id,
            recovered_count,
            recovery_reason = summary_log,
            "recovered abandoned ingestion jobs",
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Canonical ingest_job worker
// ---------------------------------------------------------------------------

async fn execute_canonical_ingest_job(
    state: Arc<AppState>,
    worker_id: &str,
    job: ingest_repository::IngestJobRow,
) -> anyhow::Result<()> {
    let job_id = job.id;
    let revision_id = job
        .knowledge_revision_id
        .context("canonical ingest job is missing knowledge_revision_id")?;
    let document_id = job
        .knowledge_document_id
        .context("canonical ingest job is missing knowledge_document_id")?;

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
                current_stage: Some("extract_content".to_string()),
            },
        )
        .await
        .context("failed to lease canonical ingest attempt")?;

    let attempt_id = attempt.id;

    let heartbeat_running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let heartbeat_flag = heartbeat_running.clone();
    let heartbeat_pg = state.persistence.postgres.clone();
    tokio::spawn(async move {
        while heartbeat_flag.load(std::sync::atomic::Ordering::Relaxed) {
            time::sleep(CANONICAL_HEARTBEAT_INTERVAL).await;
            if !heartbeat_flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let _ =
                ingest_repository::touch_attempt_heartbeat(&heartbeat_pg, attempt_id, None).await;
        }
    });

    let result = run_canonical_ingest_pipeline(
        &state,
        worker_id,
        &job,
        attempt_id,
        document_id,
        revision_id,
    )
    .await;

    heartbeat_running.store(false, std::sync::atomic::Ordering::Relaxed);

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
                        current_stage: Some("finalizing".to_string()),
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
            let message = format!("{error:#}");
            let extract_error = error.downcast_ref::<CanonicalExtractContentError>();
            let _ = state
                .canonical_services
                .ingest
                .finalize_attempt(
                    &state,
                    FinalizeAttemptCommand {
                        attempt_id,
                        knowledge_generation_id: None,
                        attempt_state: "failed".to_string(),
                        current_stage: extract_error.map(|_| "extract_content".to_string()),
                        failure_class: Some(
                            if extract_error.is_some() {
                                "content_extract"
                            } else {
                                "worker_error"
                            }
                            .to_string(),
                        ),
                        failure_code: Some(
                            extract_error
                                .map(|failure| failure.failure_code.clone())
                                .unwrap_or_else(|| "canonical_pipeline_failed".to_string()),
                        ),
                        retryable: extract_error.map(|failure| failure.retryable).unwrap_or(true),
                    },
                )
                .await;
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
) -> anyhow::Result<()> {
    // --- Stage: extract_content -----------------------------------------------
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
            let _ = state
                .canonical_services
                .extract
                .persist_extract_content(
                    state,
                    PersistExtractContentCommand {
                        revision_id,
                        attempt_id: Some(attempt_id),
                        extract_state: "failed".to_string(),
                        normalized_text: None,
                        text_checksum: None,
                        warning_count: 0,
                    },
                )
                .await;
            let _ = state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: "extract_content".to_string(),
                        stage_state: "failed".to_string(),
                        message: Some(failure_message),
                        details_json: serde_json::json!({
                            "failureCode": failure_code,
                        }),
                    },
                )
                .await;
            return Err(anyhow::Error::new(error));
        }
    };
    let text = extracted_content.text;

    let text_checksum = {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        hex::encode(hasher.finalize())
    };

    state
        .canonical_services
        .extract
        .persist_extract_content(
            state,
            PersistExtractContentCommand {
                revision_id,
                attempt_id: Some(attempt_id),
                extract_state: "ready".to_string(),
                normalized_text: Some(text.clone()),
                text_checksum: Some(text_checksum),
                warning_count: extracted_content.warning_count,
            },
        )
        .await
        .context("failed to persist extracted content")?;

    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: "extract_content".to_string(),
                stage_state: "completed".to_string(),
                message: Some("content extracted".to_string()),
                details_json: extracted_content.stage_details,
            },
        )
        .await
        .context("failed to record extract_content stage event")?;

    // --- Stage: chunk_content -------------------------------------------------
    let _ = content_repository::delete_chunks_by_revision(&state.persistence.postgres, revision_id)
        .await
        .context("failed to delete old content chunks")?;

    let _ = state
        .canonical_services
        .knowledge
        .delete_revision_chunks(state, revision_id)
        .await
        .context("failed to delete old knowledge chunks")?;

    let chunk_texts = split_text_into_chunks_with_profile(&text, ChunkingProfile::default());
    let mut next_search_char = 0usize;

    struct ChunkEntry {
        chunk_index: i32,
        start_offset: i32,
        end_offset: i32,
        token_count: Option<i32>,
        normalized_text: String,
        text_checksum: String,
    }

    let mut chunk_entries = Vec::with_capacity(chunk_texts.len());
    for (idx, chunk_text) in chunk_texts.iter().enumerate() {
        let chunk_index = i32::try_from(idx).unwrap_or(i32::MAX);
        let (start, end) = locate_chunk_span(&text, chunk_text, next_search_char);
        next_search_char = end;
        let token_count =
            Some(i32::try_from(chunk_text.split_whitespace().count()).unwrap_or(i32::MAX));
        let text_checksum = {
            let mut h = Sha256::new();
            h.update(chunk_text.as_bytes());
            hex::encode(h.finalize())
        };
        chunk_entries.push(ChunkEntry {
            chunk_index,
            start_offset: i32::try_from(start).unwrap_or(i32::MAX),
            end_offset: i32::try_from(end).unwrap_or(i32::MAX),
            token_count,
            normalized_text: chunk_text.to_string(),
            text_checksum,
        });
    }

    let postgres_chunks: Vec<_> = chunk_entries
        .iter()
        .map(|e| content_repository::NewContentChunk {
            revision_id,
            chunk_index: e.chunk_index,
            start_offset: e.start_offset,
            end_offset: e.end_offset,
            token_count: e.token_count,
            normalized_text: &e.normalized_text,
            text_checksum: &e.text_checksum,
        })
        .collect();

    let created_chunks =
        content_repository::create_chunks(&state.persistence.postgres, &postgres_chunks)
            .await
            .context("failed to create content chunks")?;

    let mut knowledge_chunks = Vec::with_capacity(created_chunks.len());
    for (chunk, entry) in created_chunks.iter().zip(chunk_entries.iter()) {
        knowledge_chunks.push(CreateKnowledgeChunkCommand {
            chunk_id: chunk.id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            document_id: revision.document_id,
            revision_id,
            chunk_index: chunk.chunk_index,
            content_text: entry.normalized_text.clone(),
            normalized_text: chunk.normalized_text.clone(),
            span_start: Some(chunk.start_offset),
            span_end: Some(chunk.end_offset),
            token_count: chunk.token_count,
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            chunk_state: "ready".to_string(),
            text_generation: Some(revision.revision_number),
            vector_generation: None,
        });
    }

    state
        .canonical_services
        .knowledge
        .write_chunks(state, knowledge_chunks)
        .await
        .context("failed to write knowledge chunks")?;

    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: "chunk_content".to_string(),
                stage_state: "completed".to_string(),
                message: Some("content chunks persisted".to_string()),
                details_json: serde_json::json!({
                    "chunkCount": created_chunks.len(),
                }),
            },
        )
        .await
        .context("failed to record chunk_content stage event")?;

    // --- Stage: embed_chunk (deferred) ----------------------------------------
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: "embed_chunk".to_string(),
                stage_state: "completed".to_string(),
                message: Some(
                    "vector stage deferred to keep background ingestion non-blocking".to_string(),
                ),
                details_json: serde_json::json!({ "strategy": "deferred_non_blocking" }),
            },
        )
        .await
        .context("failed to record embed_chunk stage event")?;

    // --- Stage: extract_graph -------------------------------------------------
    let graph_outcome =
        state.canonical_services.graph.rebuild_arango_library_graph(state, job.library_id).await;

    match &graph_outcome {
        Ok(outcome) => {
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: "extract_graph".to_string(),
                        stage_state: "completed".to_string(),
                        message: Some("graph candidates reconciled".to_string()),
                        details_json: serde_json::json!({
                            "upsertedEntities": outcome.upserted_entities,
                            "upsertedRelations": outcome.upserted_relations,
                        }),
                    },
                )
                .await
                .context("failed to record extract_graph stage event")?;
        }
        Err(graph_error) => {
            warn!(
                %worker_id,
                job_id = %job.id,
                ?graph_error,
                "canonical graph rebuild failed",
            );
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: "extract_graph".to_string(),
                        stage_state: "failed".to_string(),
                        message: Some("graph rebuild failed".to_string()),
                        details_json: serde_json::json!({
                            "error": format!("{graph_error:#}"),
                        }),
                    },
                )
                .await
                .context("failed to record extract_graph failure stage event")?;
            return Err(anyhow::anyhow!("canonical graph rebuild failed: {graph_error:#}"));
        }
    }

    // --- Stage: finalize readiness --------------------------------------------
    let now = Utc::now();
    let _ = state
        .arango_document_store
        .update_revision_readiness(
            revision_id,
            "ready",
            "ready",
            "ready",
            Some(now),
            Some(now),
            Some(now),
            revision.superseded_by_revision_id,
        )
        .await
        .context("failed to update revision readiness")?;

    // Update mutation state if a mutation is linked.
    if let Some(mutation_id) = job.mutation_id {
        let items =
            content_repository::list_mutation_items(&state.persistence.postgres, mutation_id)
                .await
                .unwrap_or_default();
        if let Some(item) = items.first() {
            let _ = content_repository::update_mutation_item(
                &state.persistence.postgres,
                item.id,
                Some(document_id),
                item.base_revision_id,
                Some(revision_id),
                "applied",
                Some("mutation applied by canonical worker"),
            )
            .await;
        }
        let _ = content_repository::update_mutation_status(
            &state.persistence.postgres,
            mutation_id,
            "applied",
            Some(Utc::now()),
            None,
            None,
        )
        .await;
    }

    // Promote the document head to this revision.
    let _ = content_repository::upsert_document_head(
        &state.persistence.postgres,
        &content_repository::NewContentDocumentHead {
            document_id,
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            latest_mutation_id: job.mutation_id,
            latest_successful_attempt_id: Some(attempt_id),
        },
    )
    .await;

    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: "finalizing".to_string(),
                stage_state: "completed".to_string(),
                message: Some("canonical ingest pipeline completed".to_string()),
                details_json: serde_json::json!({
                    "revisionId": revision_id,
                    "documentId": document_id,
                }),
            },
        )
        .await
        .context("failed to record finalizing stage event")?;

    Ok(())
}

async fn resolve_canonical_extract_content(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    revision: &KnowledgeRevisionRow,
) -> Result<CanonicalExtractedContent, CanonicalExtractContentError> {
    if let Some(text) = revision
        .normalized_text
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
    {
        return Ok(CanonicalExtractedContent {
            text: text.clone(),
            warning_count: 0,
            stage_details: serde_json::json!({
                "contentLength": text.chars().count(),
                "source": "knowledge_revision",
            }),
        });
    }

    let storage_ref = match revision
        .storage_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(storage_ref) => storage_ref.to_string(),
        None => state
            .canonical_services
            .content
            .resolve_revision_storage_key(state, revision.revision_id)
            .await
            .map_err(|_| {
                CanonicalExtractContentError::missing_stored_source(job.id, revision.revision_id)
            })?
            .ok_or_else(|| {
                CanonicalExtractContentError::missing_stored_source(job.id, revision.revision_id)
            })?,
    };
    let stored_bytes =
        state.content_storage.read_revision_source(&storage_ref).await.map_err(|error| {
            CanonicalExtractContentError::stored_source_read(&storage_ref, error)
        })?;
    let file_name = canonical_revision_file_name(revision);
    let plan = state
        .canonical_services
        .content
        .build_runtime_extraction_plan(
            state,
            revision.library_id,
            &file_name,
            Some(revision.mime_type.as_str()),
            &stored_bytes,
        )
        .await
        .map_err(|rejection| CanonicalExtractContentError::extraction_rejected(&rejection))?;
    let text = plan.extracted_text.unwrap_or_default();
    let warning_count = i32::try_from(plan.extraction_warnings.len()).unwrap_or(i32::MAX);
    Ok(CanonicalExtractedContent {
        text: text.clone(),
        warning_count,
        stage_details: serde_json::json!({
            "contentLength": text.chars().count(),
            "fileKind": plan.file_kind.as_str(),
            "warningCount": plan.extraction_warnings.len(),
            "source": "content_storage",
            "storageRef": storage_ref,
        }),
    })
}

fn canonical_revision_file_name(revision: &KnowledgeRevisionRow) -> String {
    let source_name = revision
        .source_uri
        .as_deref()
        .and_then(|value| value.split_once("://").map(|(_, rest)| rest).or(Some(value)))
        .and_then(|value| value.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "inline")
        .map(str::to_string);
    source_name
        .or_else(|| {
            revision
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("revision-{}", revision.revision_id))
}

fn locate_chunk_span(text: &str, chunk_text: &str, next_search_char: usize) -> (usize, usize) {
    fn char_to_byte(text: &str, char_offset: usize) -> usize {
        text.char_indices().nth(char_offset).map_or(text.len(), |(i, _)| i)
    }
    let start_byte = char_to_byte(text, next_search_char);
    if let Some(rel) = text[start_byte..].find(chunk_text) {
        let begin = start_byte + rel;
        let end = begin + chunk_text.len();
        (text[..begin].chars().count(), text[..end].chars().count())
    } else {
        (next_search_char, next_search_char + chunk_text.chars().count())
    }
}

async fn fail_canonical_ingest_job(
    state: &AppState,
    job_id: Uuid,
    worker_id: &str,
    error: &anyhow::Error,
) {
    let message = format!("{error:#}");
    let existing = match ingest_repository::get_ingest_job_by_id(
        &state.persistence.postgres,
        job_id,
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            error!(%worker_id, %job_id, "canonical ingest job vanished while trying to fail it");
            return;
        }
        Err(db_error) => {
            error!(%worker_id, %job_id, ?db_error, "failed to load canonical ingest job for failure");
            return;
        }
    };

    if existing.queue_state == "completed" {
        return;
    }

    if existing.queue_state != "failed" {
        let update_result = ingest_repository::update_ingest_job(
            &state.persistence.postgres,
            job_id,
            &ingest_repository::UpdateIngestJob {
                mutation_id: existing.mutation_id,
                connector_id: existing.connector_id,
                async_operation_id: existing.async_operation_id,
                knowledge_document_id: existing.knowledge_document_id,
                knowledge_revision_id: existing.knowledge_revision_id,
                job_kind: existing.job_kind.clone(),
                queue_state: "failed".to_string(),
                priority: existing.priority,
                dedupe_key: existing.dedupe_key.clone(),
                available_at: existing.available_at,
                completed_at: Some(Utc::now()),
            },
        )
        .await;
        if let Err(db_error) = update_result {
            error!(
                %worker_id,
                %job_id,
                ?db_error,
                original_error = %message,
                "failed to mark canonical ingest job as failed",
            );
        }
    }

    let failure_code = latest_canonical_attempt_failure_code(state, job_id)
        .await
        .unwrap_or_else(|| "canonical_pipeline_failed".to_string());
    if let Some(mutation_id) = existing.mutation_id
        && let Err(reconcile_error) = state
            .canonical_services
            .content
            .reconcile_failed_ingest_mutation(
                state,
                crate::services::content_service::ReconcileFailedIngestMutationCommand {
                    mutation_id,
                    failure_code,
                    failure_message: message.clone(),
                },
            )
            .await
    {
        error!(
            %worker_id,
            %job_id,
            ?reconcile_error,
            original_error = %message,
            "failed to reconcile canonical content mutation after ingest failure",
        );
    }
}

async fn latest_canonical_attempt_failure_code(state: &AppState, job_id: Uuid) -> Option<String> {
    ingest_repository::get_latest_ingest_attempt_by_job(&state.persistence.postgres, job_id)
        .await
        .ok()
        .flatten()
        .and_then(|attempt| attempt.failure_code)
}
