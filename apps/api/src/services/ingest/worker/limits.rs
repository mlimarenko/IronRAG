use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    infra::repositories::content_revision_repository,
    integrations::docling,
    shared::{
        admission::{PermitWaitError, acquire_permit_within},
        telemetry,
    },
};

const DEFAULT_HEAVY_REVISION_BYTES: i64 = 8 * 1024 * 1024;
const HEAVY_PIPELINE_AUTO_MAX_PARALLELISM: usize = 4;
const HEAVY_PIPELINE_AUTO_RESERVED_MEMORY_MIB: u64 = 2048;
const HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB: u64 = 1024;

static HEAVY_REVISION_PIPELINE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(heavy_revision_pipeline_parallelism())));

/// Typed refusal when a heavy revision cannot get a pipeline slot within the
/// configured bound. Retryable: the slot frees when a running heavy job
/// finishes, so a requeue can succeed without operator action.
#[derive(Debug, Error)]
#[error("heavy revision pipeline permit wait timed out after {timeout_ms} ms")]
pub(super) struct HeavyRevisionPipelinePermitTimeout {
    pub(super) timeout_ms: u64,
}

pub(super) async fn acquire_heavy_revision_pipeline_slot(
    revision: &content_revision_repository::ContentRevisionRow,
) -> anyhow::Result<Option<OwnedSemaphorePermit>> {
    if !is_heavy_revision_pipeline_job(revision) {
        return Ok(None);
    }
    let permit = acquire_heavy_revision_pipeline_slot_within(
        &HEAVY_REVISION_PIPELINE,
        Duration::from_secs(heavy_revision_acquire_timeout_secs()),
    )
    .await?;
    Ok(Some(permit))
}

pub(super) async fn acquire_heavy_revision_pipeline_slot_within(
    semaphore: &Arc<Semaphore>,
    timeout: Duration,
) -> anyhow::Result<OwnedSemaphorePermit> {
    acquire_permit_within(Arc::clone(semaphore), timeout, "ingest_heavy_revision")
        .await
        .map_err(heavy_revision_permit_error)
}

fn heavy_revision_permit_error(error: PermitWaitError) -> anyhow::Error {
    match error {
        PermitWaitError::LimiterClosed { .. } => {
            anyhow::anyhow!("heavy revision pipeline limiter is closed")
        }
        PermitWaitError::AcquireTimeout { timeout_ms, .. } => {
            anyhow::Error::new(HeavyRevisionPipelinePermitTimeout { timeout_ms })
        }
    }
}

/// Bounded wait for one heavy-revision pipeline slot. Override with
/// `IRONRAG_INGESTION_HEAVY_ACQUIRE_TIMEOUT_SECS`; the default matches Docling
/// because one pipeline slot is backed by one Docling queue.
fn heavy_revision_acquire_timeout_secs() -> u64 {
    std::env::var("IRONRAG_INGESTION_HEAVY_ACQUIRE_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(docling::default_docling_acquire_timeout_secs)
}

fn is_heavy_revision_pipeline_job(
    revision: &content_revision_repository::ContentRevisionRow,
) -> bool {
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

#[derive(Debug, PartialEq, Eq)]
pub(super) enum HeavyPipelineParallelismSetting<'a> {
    Auto,
    Configured(usize),
    Invalid(&'a str),
}

pub(super) fn heavy_pipeline_parallelism_setting(
    raw: Option<&str>,
) -> HeavyPipelineParallelismSetting<'_> {
    let Some(value) = raw.map(str::trim) else {
        return HeavyPipelineParallelismSetting::Auto;
    };
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return HeavyPipelineParallelismSetting::Auto;
    }
    match value.parse::<usize>().ok().filter(|value| *value > 0) {
        Some(value) => HeavyPipelineParallelismSetting::Configured(value),
        None => HeavyPipelineParallelismSetting::Invalid(value),
    }
}

fn heavy_revision_pipeline_parallelism() -> usize {
    let raw = std::env::var("IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM").ok();
    match heavy_pipeline_parallelism_setting(raw.as_deref()) {
        HeavyPipelineParallelismSetting::Auto => auto_heavy_revision_pipeline_parallelism(),
        HeavyPipelineParallelismSetting::Configured(value) => {
            tracing::info!(parallelism = value, "heavy revision pipeline parallelism configured");
            value
        }
        HeavyPipelineParallelismSetting::Invalid(value) => {
            invalid_heavy_pipeline_parallelism(value)
        }
    }
}

fn invalid_heavy_pipeline_parallelism(value: &str) -> usize {
    tracing::warn!(
        raw = value,
        fallback_parallelism = 1,
        "invalid IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM; using fail-safe heavy revision pipeline parallelism"
    );
    1
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
        docling_waiters_per_process = docling::DOCLING_WAITERS_PER_PROCESS,
        parallelism,
        "heavy revision pipeline auto parallelism resolved"
    );
    if heavy_budget_mib.is_some_and(|budget| budget < HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB) {
        warn_insufficient_heavy_pipeline_budget(memory_limit_mib, soft_limit_mib, heavy_budget_mib);
    }
    parallelism
}

fn warn_insufficient_heavy_pipeline_budget(
    memory_limit_mib: Option<u64>,
    soft_limit_mib: Option<u64>,
    heavy_budget_mib: Option<u64>,
) {
    tracing::warn!(
        ?memory_limit_mib,
        ?soft_limit_mib,
        ?heavy_budget_mib,
        required_mib = HEAVY_PIPELINE_AUTO_MEMORY_PER_JOB_MIB,
        "heavy revision pipeline auto parallelism has only enough memory budget for the mandatory single job"
    );
}

pub(super) fn auto_heavy_revision_pipeline_parallelism_for_limits(
    cpu_parallelism: usize,
    memory_limit_bytes: Option<u64>,
    docling_parallelism: usize,
) -> usize {
    let cpu_bound = cpu_parallelism.clamp(1, HEAVY_PIPELINE_AUTO_MAX_PARALLELISM);
    let docling_bound = docling_parallelism
        .max(1)
        .saturating_mul(docling::DOCLING_WAITERS_PER_PROCESS)
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
