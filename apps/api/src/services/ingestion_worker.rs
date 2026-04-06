use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::{sync::broadcast, task::JoinHandle, time};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        arangodb::document_store::KnowledgeRevisionRow,
        repositories::{content_repository, ingest_repository},
    },
    services::{
        content_service::{MaterializeRevisionGraphCandidatesCommand, PromoteHeadCommand},
        ingest_service::{
            FinalizeAttemptCommand, INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EMBED_CHUNK,
            INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH,
            INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_FINALIZING,
            INGEST_STAGE_PREPARE_STRUCTURE, INGEST_STAGE_WEB_DISCOVERY,
            INGEST_STAGE_WEB_MATERIALIZE_PAGE, LeaseAttemptCommand, RecordStageEventCommand,
        },
    },
    shared::file_extract::{
        FileExtractionPlan, UploadAdmissionError, build_inline_text_extraction_plan,
    },
};

const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(2);
const CANONICAL_LEASE_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);
const CANONICAL_STALE_LEASE_SECONDS: i64 = 120;
const CANONICAL_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

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
    extraction_plan: FileExtractionPlan,
    stage_details: serde_json::Value,
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
    let worker_concurrency = state.settings.ingestion_worker_concurrency.max(1);

    let mut handles = Vec::new();

    info!(worker_concurrency, "starting ingestion worker pool on the canonical queue only");

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
                            fail_canonical_ingest_job(&state, job_id, &worker_id, &error).await;
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

async fn latest_canonical_attempt_failure_code(state: &AppState, job_id: Uuid) -> Option<String> {
    ingest_repository::get_latest_ingest_attempt_by_job(&state.persistence.postgres, job_id)
        .await
        .ok()
        .flatten()
        .and_then(|attempt| attempt.failure_code)
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

    let failure_code = latest_canonical_attempt_failure_code(state, job_id).await.unwrap_or_else(
        || match existing.job_kind.as_str() {
            "web_discovery" => "web_discovery_failed".to_string(),
            "web_materialize_page" => "web_materialize_page_failed".to_string(),
            _ => "canonical_pipeline_failed".to_string(),
        },
    );
    if existing.job_kind == "web_discovery" {
        match resolve_canonical_job_subject_id(state, &existing, "content_web_ingest_run").await {
            Ok(run_id) => {
                if let Err(reconcile_error) = state
                    .canonical_services
                    .web_ingest
                    .fail_recursive_discovery_job(state, run_id, &failure_code)
                    .await
                {
                    error!(
                        %worker_id,
                        %job_id,
                        %run_id,
                        ?reconcile_error,
                        original_error = %message,
                        "failed to reconcile recursive discovery job failure",
                    );
                }
            }
            Err(resolve_error) => {
                error!(
                    %worker_id,
                    %job_id,
                    ?resolve_error,
                    original_error = %message,
                    "failed to resolve recursive discovery run subject",
                );
            }
        }
        return;
    }
    if existing.job_kind == "web_materialize_page" {
        match resolve_canonical_job_subject_id(state, &existing, "content_web_discovered_page")
            .await
        {
            Ok(candidate_id) => {
                if let Err(reconcile_error) = state
                    .canonical_services
                    .web_ingest
                    .fail_recursive_page_job(state, candidate_id, &failure_code)
                    .await
                {
                    error!(
                        %worker_id,
                        %job_id,
                        %candidate_id,
                        ?reconcile_error,
                        original_error = %message,
                        "failed to reconcile recursive page job failure",
                    );
                }
            }
            Err(resolve_error) => {
                error!(
                    %worker_id,
                    %job_id,
                    ?resolve_error,
                    original_error = %message,
                    "failed to resolve recursive page subject",
                );
            }
        }
        return;
    }
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

    let result = match job.job_kind.as_str() {
        "content_mutation" => {
            let revision_id = job
                .knowledge_revision_id
                .context("canonical ingest job is missing knowledge_revision_id")?;
            let document_id = job
                .knowledge_document_id
                .context("canonical ingest job is missing knowledge_document_id")?;
            run_canonical_ingest_pipeline(
                &state,
                worker_id,
                &job,
                attempt_id,
                document_id,
                revision_id,
            )
            .await
        }
        "web_discovery" => run_canonical_web_discovery_job(&state, &job, attempt_id).await,
        "web_materialize_page" => {
            run_canonical_web_materialize_page_job(&state, &job, attempt_id).await
        }
        other => Err(anyhow::anyhow!("unsupported canonical ingest job kind {other}")),
    };

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
                        current_stage: Some(match job.job_kind.as_str() {
                            "content_mutation" => INGEST_STAGE_FINALIZING.to_string(),
                            "web_discovery" => INGEST_STAGE_WEB_DISCOVERY.to_string(),
                            "web_materialize_page" => INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
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
                .await;
            Err(error).context(message)
        }
    }
}

async fn run_canonical_web_discovery_job(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
) -> anyhow::Result<()> {
    let run_id = resolve_canonical_job_subject_id(state, job, "content_web_ingest_run").await?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_WEB_DISCOVERY.to_string(),
                stage_state: "started".to_string(),
                message: Some("discovering recursive crawl scope".to_string()),
                details_json: serde_json::json!({ "runId": run_id }),
            },
        )
        .await
        .context("failed to record web_discovery start stage event")?;
    match state.canonical_services.web_ingest.execute_recursive_discovery_job(state, run_id).await {
        Ok(()) => {
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_DISCOVERY.to_string(),
                        stage_state: "completed".to_string(),
                        message: Some(
                            "recursive crawl scope closed and page jobs queued".to_string(),
                        ),
                        details_json: serde_json::json!({ "runId": run_id }),
                    },
                )
                .await
                .context("failed to record web_discovery stage event")?;
            Ok(())
        }
        Err(error) => {
            let error_message = error.to_string();
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_DISCOVERY.to_string(),
                        stage_state: "failed".to_string(),
                        message: Some("recursive crawl discovery failed".to_string()),
                        details_json: serde_json::json!({
                            "runId": run_id,
                            "error": error_message,
                        }),
                    },
                )
                .await
                .context("failed to record web_discovery failure stage event")?;
            Err(anyhow::anyhow!("web discovery job failed: {}", error))
        }
    }
}

async fn run_canonical_web_materialize_page_job(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    attempt_id: Uuid,
) -> anyhow::Result<()> {
    let candidate_id =
        resolve_canonical_job_subject_id(state, job, "content_web_discovered_page").await?;
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
                stage_state: "started".to_string(),
                message: Some("materializing discovered page from stored snapshot".to_string()),
                details_json: serde_json::json!({ "candidateId": candidate_id }),
            },
        )
        .await
        .context("failed to record web_materialize_page start stage event")?;
    match state.canonical_services.web_ingest.execute_recursive_page_job(state, candidate_id).await
    {
        Ok(()) => {
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
                        stage_state: "completed".to_string(),
                        message: Some("discovered page materialized".to_string()),
                        details_json: serde_json::json!({ "candidateId": candidate_id }),
                    },
                )
                .await
                .context("failed to record web_materialize_page stage event")?;
            Ok(())
        }
        Err(error) => {
            let error_message = error.to_string();
            state
                .canonical_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand {
                        attempt_id,
                        stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE.to_string(),
                        stage_state: "failed".to_string(),
                        message: Some("discovered page materialization failed".to_string()),
                        details_json: serde_json::json!({
                            "candidateId": candidate_id,
                            "error": error_message,
                        }),
                    },
                )
                .await
                .context("failed to record web_materialize_page failure stage event")?;
            Err(anyhow::anyhow!("web page materialization job failed: {}", error))
        }
    }
}

async fn resolve_canonical_job_subject_id(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    expected_subject_kind: &str,
) -> anyhow::Result<Uuid> {
    let operation_id =
        job.async_operation_id.context("canonical web ingest job is missing async_operation_id")?;
    let operation = state
        .canonical_services
        .ops
        .get_async_operation(state, operation_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let subject_kind = operation
        .subject_kind
        .as_deref()
        .context("canonical web ingest job subject_kind is missing")?;
    let subject_id =
        operation.subject_id.context("canonical web ingest job subject_id is missing")?;
    if subject_kind != expected_subject_kind {
        anyhow::bail!(
            "canonical web ingest job subject kind mismatch: expected {}, found {}",
            expected_subject_kind,
            subject_kind
        );
    }
    Ok(subject_id)
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
                .knowledge
                .set_revision_extract_state(state, revision_id, "failed", None, None)
                .await;
            let _ = state
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
                    },
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
            },
        )
        .await
        .context("failed to record extract_content stage event")?;

    // --- Stage: prepare_structure / chunk_content / extract_technical_facts ---
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
                stage_state: "completed".to_string(),
                message: Some("content chunks persisted".to_string()),
                details_json: serde_json::json!({
                    "chunkCount": preparation.chunk_count,
                }),
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
                stage_state: "completed".to_string(),
                message: Some("technical facts extracted from structured revision".to_string()),
                details_json: serde_json::json!({
                    "technicalFactCount": preparation.technical_fact_count,
                    "technicalConflictCount": preparation.technical_conflict_count,
                }),
            },
        )
        .await
        .context("failed to record extract_technical_facts stage event")?;

    // --- Stage: embed_chunk (deferred) ----------------------------------------
    state
        .canonical_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
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
            },
        )
        .await
        .context("failed to record extract_graph start stage event")?;
    let graph_materialization = state
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
        )
        .await;
    let mut graph_ready = false;

    match graph_materialization {
        Ok(graph_materialization) => {
            let graph_outcome = state
                .canonical_services
                .graph
                .reconcile_revision_graph(
                    state,
                    job.library_id,
                    document_id,
                    revision_id,
                    Some(attempt_id),
                )
                .await;
            graph_ready = graph_outcome.as_ref().is_ok_and(|outcome| outcome.graph_ready);

            match graph_outcome {
                Ok(outcome) => {
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
                                    "extractedEntityCandidates": graph_materialization.extracted_entities,
                                    "extractedRelationCandidates": graph_materialization.extracted_relations,
                                    "projectedNodes": outcome.projection.node_count,
                                    "projectedEdges": outcome.projection.edge_count,
                                    "projectionVersion": outcome.projection.projection_version,
                                    "graphStatus": outcome.projection.graph_status,
                                    "graphContributionCount": outcome.graph_contribution_count,
                                    "graphReady": graph_ready,
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
                        revision_id = %revision_id,
                        ?graph_error,
                        "canonical graph rebuild failed; preserving readable revision",
                    );
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
                                    "extractedEntityCandidates": graph_materialization.extracted_entities,
                                    "extractedRelationCandidates": graph_materialization.extracted_relations,
                                    "graphReady": false,
                                    "degradedToReadable": true,
                                    "error": format!("{graph_error:#}"),
                                }),
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
                    },
                )
                .await
                .context("failed to record extract_graph extraction failure stage event")?;
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
            if graph_ready { "ready" } else { "processing" },
            Some(now),
            Some(now),
            graph_ready.then_some(now),
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

    // Promote the document head through the canonical service so Postgres and Arango stay aligned.
    let _ = state
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
        .await;
    state
        .canonical_services
        .content
        .converge_document_technical_facts(state, document_id, Some(revision_id))
        .await
        .context("failed to converge typed technical facts for current revision")?;

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
        let extraction_plan = build_inline_text_extraction_plan(&text);
        return Ok(CanonicalExtractedContent {
            extraction_plan,
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
    let text = plan.normalized_text.clone().unwrap_or_default();
    Ok(CanonicalExtractedContent {
        extraction_plan: plan.clone(),
        stage_details: serde_json::json!({
            "contentLength": text.chars().count(),
            "fileKind": plan.file_kind.as_str(),
            "warningCount": plan.extraction_warnings.len(),
            "lineCount": plan.source_format_metadata.line_count,
            "pageCount": plan.source_format_metadata.page_count,
            "normalizationProfile": plan.normalization_profile,
            "source": "content_storage",
            "storageRef": storage_ref,
        }),
    })
}
