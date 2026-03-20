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
    domains::pricing_catalog::{PricingBillingUnit, PricingCapability, PricingResolutionStatus},
    infra::repositories::{self, IngestionJobRow},
    services::{
        document_accounting,
        document_reconciliation::{advance_project_source_truth, persist_mutation_impact_scope},
        graph_extract::{
            GraphExtractionRecoveryRecord, GraphExtractionRequest, GraphExtractionResumeHint,
            GraphExtractionTelemetrySummary, extract_and_persist_chunk_graph_result,
            extraction_outcome_from_resume_state, summarize_graph_extraction_usage_calls,
        },
        graph_merge::{
            GraphMergeScope, merge_chunk_graph_candidates, reconcile_merge_support_counts,
        },
        graph_projection::{project_canonical_graph, resolve_projection_scope},
        graph_summary::GraphSummaryRefreshRequest,
        runtime_ingestion::{
            JobLeaseHeartbeat, RuntimeStageUsageSummary, embed_runtime_chunks,
            embed_runtime_graph_edges, embed_runtime_graph_nodes,
            persist_extracted_content_from_payload, persist_library_queue_isolation_waiting_reason,
            refresh_library_collection_settlement_snapshots, refresh_library_warning_snapshots,
            release_library_queue_isolation_slot, resolve_runtime_run_provider_profile,
            upsert_runtime_document_chunk_contribution_summary,
            upsert_runtime_document_graph_contribution_summary,
        },
    },
    shared::chunking::{ChunkingProfile, split_text_into_chunks_with_profile},
};

const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_WORKER_LEASE_DURATION: Duration = Duration::from_secs(300);
const DEFAULT_WORKER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_STALE_WORKER_GRACE_SECONDS: i64 = 45;
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

async fn run_ingestion_worker_pool(state: Arc<AppState>, shutdown: broadcast::Receiver<()>) {
    let worker_concurrency = state.settings.ingestion_worker_concurrency.max(1);
    info!(worker_concurrency, "starting ingestion worker pool");

    let mut handles = Vec::with_capacity(worker_concurrency + 1);
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
    let provider_profile = runtime_run
        .as_ref()
        .map(|row| resolve_runtime_run_provider_profile(state.as_ref(), row))
        .unwrap_or_else(|| state.effective_provider_profile());
    if let Some(runtime_ingestion_run_id) = runtime_ingestion_run_id {
        repositories::mark_runtime_ingestion_run_claimed(
            &state.persistence.postgres,
            runtime_ingestion_run_id,
            Utc::now(),
        )
        .await
        .context("failed to mark runtime ingestion run as claimed")?;
        persist_library_queue_isolation_waiting_reason(state.as_ref(), job.project_id)
            .await
            .context("failed to persist queue-isolation waiting reason after runtime claim")?;
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
        &payload,
        runtime_ingestion_run_id,
        mutation_document,
        previous_active_revision,
        &checksum,
    )
    .await?;
    let document = document_context.document.clone();
    let document_for_processing = document_context.document_for_processing.clone();

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

    if merge_follow_up_required {
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
        let source_truth_version = advance_project_source_truth(state.as_ref(), payload.project_id)
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
    let terminal_status =
        if graph_contribution_count > 0 && projection_outcome.graph_status == "ready" {
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
    release_library_queue_isolation_slot(state.as_ref(), job.project_id)
        .await
        .context("failed to release queue-isolation slot after job completion")?;
    finalize_document_attempt_success(state.as_ref(), &payload, &document_context, terminal_status)
        .await?;
    refresh_terminal_library_settlement(state.as_ref(), job.project_id)
        .await
        .context("failed to refresh final collection settlement after job completion")?;

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
    payload: &repositories::IngestionExecutionPayload,
    runtime_ingestion_run_id: Option<Uuid>,
    existing_document: Option<repositories::DocumentRow>,
    previous_active_revision: Option<repositories::DocumentRevisionRow>,
    checksum: &str,
) -> anyhow::Result<WorkerDocumentContext> {
    if let Some(document) = existing_document {
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
                payload.target_revision_id.or(document.current_revision_id),
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
        return Ok(WorkerDocumentContext {
            document,
            document_for_processing,
            target_revision_id: payload.target_revision_id,
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

    Ok(WorkerDocumentContext {
        document: document.clone(),
        document_for_processing: document,
        target_revision_id: Some(target_revision.id),
        previous_active_revision: None,
        old_chunk_ids: Vec::new(),
    })
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
        deleted_query_refs = repositories::delete_runtime_query_references_by_document_revision(
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
        persist_mutation_impact_scope(state, mutation_workflow_id, &detected_scope).await?;
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
        repositories::delete_runtime_query_references_by_document_revision(
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
    let source_truth_version = advance_project_source_truth(state, payload.project_id)
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

async fn refresh_terminal_library_settlement(
    state: &AppState,
    project_id: Uuid,
) -> anyhow::Result<()> {
    refresh_library_collection_settlement_snapshots(state, project_id).await?;
    refresh_library_warning_snapshots(state, project_id).await
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
    if let Some(project_id) = runtime_stage_snapshot.as_ref().map(|run| run.project_id) {
        if let Err(queue_error) = release_library_queue_isolation_slot(state, project_id).await {
            error!(
                job_id = %job_id,
                %worker_id,
                project_id = %project_id,
                ?queue_error,
                "failed to release queue-isolation slot after job failure"
            );
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
                } else if let Err(settlement_error) =
                    refresh_terminal_library_settlement(state, job.project_id).await
                {
                    error!(
                        job_id = %job_id,
                        %worker_id,
                        project_id = %job.project_id,
                        ?settlement_error,
                        "failed to refresh final collection settlement after ingestion failure"
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
        "refreshing Neo4j after a delete or reprocess mutation"
    } else {
        "projecting canonical graph into Neo4j"
    }
}

fn projecting_graph_completed_message(rebuild_follow_up: bool, graph_status: &str) -> &'static str {
    match (rebuild_follow_up, graph_status) {
        (_, "ready") if rebuild_follow_up => "stale graph projection refreshed in Neo4j",
        (_, "ready") => "Neo4j projection refreshed",
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
    use std::sync::Arc;

    use super::*;
    use crate::{
        app::{config::Settings, state::AppState},
        infra::repositories::{self, IngestionJobRow},
        services::runtime_ingestion::{
            QueueRuntimeUploadRequest, RuntimeUploadFileInput, queue_new_runtime_upload,
        },
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
    #[ignore = "requires local postgres, redis, and neo4j services"]
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

    #[tokio::test]
    #[ignore = "requires local postgres, redis, and neo4j services"]
    async fn runtime_worker_progresses_run_to_ready_no_graph() {
        let state =
            AppState::new(Settings::from_env().expect("settings")).await.expect("app state");
        let slug = format!("rt-stage-{}", Uuid::now_v7().simple());
        let workspace = repositories::create_workspace(
            &state.persistence.postgres,
            &slug,
            "Runtime Stage Test Workspace",
        )
        .await
        .expect("workspace");
        let project = repositories::create_project(
            &state.persistence.postgres,
            workspace.id,
            &format!("lib-{}", Uuid::now_v7().simple()),
            "Runtime Stage Test Library",
            Some("runtime ingestion stage progression test"),
        )
        .await
        .expect("project");
        let queued = queue_new_runtime_upload(
            &state,
            QueueRuntimeUploadRequest {
                project_id: project.id,
                upload_batch_id: Some(Uuid::now_v7()),
                requested_by: Some("test@rustrag.local".to_string()),
                trigger_kind: "runtime_test_upload".to_string(),
                parent_job_id: None,
                idempotency_key: None,
                file: RuntimeUploadFileInput {
                    source_id: None,
                    file_name: "runtime-stage.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    file_bytes: b"Entity extraction begins here.\n\nChunked context follows."
                        .to_vec(),
                    title: Some("Runtime stage test".to_string()),
                },
            },
        )
        .await
        .expect("queue runtime upload");

        execute_job(Arc::new(state.clone()), "test-worker", queued.ingestion_job.clone())
            .await
            .expect("execute worker job");

        let run = repositories::get_runtime_ingestion_run_by_id(
            &state.persistence.postgres,
            queued.runtime_run.id,
        )
        .await
        .expect("load runtime run")
        .expect("runtime run exists");
        assert_eq!(run.status, "ready_no_graph");
        assert_eq!(run.current_stage, "finalizing");
        assert_eq!(run.progress_percent, Some(100));

        let extracted = repositories::get_runtime_extracted_content_by_run(
            &state.persistence.postgres,
            queued.runtime_run.id,
        )
        .await
        .expect("load extracted content")
        .expect("extracted content exists");
        assert_eq!(extracted.extraction_kind, "text_like");
        assert!(
            extracted
                .content_text
                .as_deref()
                .is_some_and(|text| text.contains("Entity extraction"))
        );

        let events = repositories::list_runtime_stage_events_by_run(
            &state.persistence.postgres,
            queued.runtime_run.id,
        )
        .await
        .expect("load stage events");
        let stage_pairs = events
            .into_iter()
            .map(|event| format!("{}:{}", event.stage, event.status))
            .collect::<Vec<_>>();
        assert!(stage_pairs.iter().any(|value| value == "accepted:completed"));
        assert!(stage_pairs.iter().any(|value| value == "extracting_content:started"));
        assert!(stage_pairs.iter().any(|value| value == "extracting_content:completed"));
        assert!(stage_pairs.iter().any(|value| value == "chunking:started"));
        assert!(stage_pairs.iter().any(|value| value == "chunking:completed"));
        assert!(stage_pairs.iter().any(|value| value == "finalizing:started"));
        assert!(stage_pairs.iter().any(|value| value == "finalizing:completed"));

        let document_id = run.document_id.expect("document persisted");
        let chunks =
            repositories::list_chunks_by_document(&state.persistence.postgres, document_id)
                .await
                .expect("load chunks");
        assert!(!chunks.is_empty());
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
        if let Err(queue_error) = release_library_queue_isolation_slot(state, job.project_id).await
        {
            warn!(
                %worker_id,
                project_id = %job.project_id,
                ?queue_error,
                "failed to release queue-isolation slot during recovery"
            );
        } else if let Err(settlement_error) =
            refresh_terminal_library_settlement(state, job.project_id).await
        {
            warn!(
                %worker_id,
                project_id = %job.project_id,
                ?settlement_error,
                "failed to refresh final collection settlement after recovery requeue"
            );
        }
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
