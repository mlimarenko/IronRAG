use std::{sync::Arc, time::Duration};

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokio::{sync::broadcast, task::JoinHandle, time};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, IngestionJobRow},
    shared::chunking::split_text_into_chunks,
};

const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(2);
const WORKER_LEASE_DURATION: Duration = Duration::from_secs(30);

pub fn spawn_ingestion_worker(
    state: AppState,
    shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_ingestion_worker(Arc::new(state), shutdown).await;
    })
}

async fn run_ingestion_worker(state: Arc<AppState>, mut shutdown: broadcast::Receiver<()>) {
    let worker_id = format!("backend:{}", Uuid::now_v7());
    info!(%worker_id, "starting ingestion worker loop");

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!(%worker_id, "stopping ingestion worker loop");
                break;
            }
            _ = time::sleep(WORKER_POLL_INTERVAL) => {
                if let Err(error) = recover_expired_leases(state.as_ref(), &worker_id).await {
                    warn!(%worker_id, ?error, "failed to recover expired ingestion job leases");
                }

                match repositories::claim_next_ingestion_job(
                    &state.persistence.postgres,
                    &worker_id,
                    chrono::Duration::from_std(WORKER_LEASE_DURATION).unwrap_or_else(|_| chrono::Duration::seconds(30)),
                ).await {
                    Ok(Some(job)) => {
                        let job_id = job.id;
                        let attempt_no = job.attempt_count;
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
                            error!(%worker_id, job_id=%job_id, ?error, "ingestion worker job execution crashed");
                            fail_job(&state, job_id, Some(attempt_no), &worker_id, &error).await;
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
    let payload = repositories::parse_ingestion_execution_payload(&job)
        .context("ingestion job payload missing or invalid")?;

    info!(
        job_id = %job.id,
        %worker_id,
        project_id = %payload.project_id,
        source_id = ?payload.source_id,
        attempt_no,
        external_key = %payload.external_key,
        ingest_mode = %payload.ingest_mode,
        text_len = payload.text.len(),
        "starting ingestion job",
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

    let checksum = sha256_hex(&payload.text);
    let document = repositories::create_document(
        &state.persistence.postgres,
        payload.project_id,
        payload.source_id,
        &payload.external_key,
        payload.title.as_deref(),
        payload.mime_type.as_deref(),
        Some(&checksum),
    )
    .await?;

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

    let chunks = split_text_into_chunks(&payload.text, 1200);
    let mut chunk_count = 0usize;
    for (idx, content) in chunks.iter().enumerate() {
        repositories::create_chunk(
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
            }),
        )
        .await?;
        chunk_count += 1;
    }

    repositories::complete_ingestion_job(
        &state.persistence.postgres,
        job.id,
        worker_id,
        serde_json::json!({
            "document_id": document.id,
            "chunk_count": chunk_count,
            "checksum": checksum,
            "attempt_no": attempt_no,
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

    info!(job_id=%job.id, %worker_id, document_id=%document.id, chunk_count, "completed ingestion job");
    Ok(())
}

pub async fn fail_job(
    state: &AppState,
    job_id: Uuid,
    attempt_no: Option<i32>,
    worker_id: &str,
    error: &anyhow::Error,
) {
    let message = error.to_string();
    error!(job_id=%job_id, %worker_id, attempt_no, error=%message, "ingestion job failed");

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
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

async fn recover_expired_leases(state: &AppState, worker_id: &str) -> anyhow::Result<()> {
    let recovered =
        repositories::recover_expired_ingestion_job_leases(&state.persistence.postgres).await?;
    for job in recovered {
        if job.attempt_count > 0 {
            repositories::fail_ingestion_job_attempt(
                &state.persistence.postgres,
                job.id,
                job.attempt_count,
                job.worker_id.as_deref().unwrap_or(worker_id),
                "lease_expired",
                "job lease expired before completion; requeued for retry",
            )
            .await?;
        }
        warn!(job_id=%job.id, previous_worker_id=?job.worker_id, attempt_no=job.attempt_count, "requeued abandoned ingestion job after lease expiry");
    }
    Ok(())
}
