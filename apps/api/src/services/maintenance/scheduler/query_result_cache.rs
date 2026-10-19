use std::{sync::OnceLock, time::Duration};

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Gauge},
};
use tracing::{info, warn};

use crate::{app::state::AppState, infra::repositories::query_result_cache_repository};

pub(super) const QUERY_RESULT_CACHE_GC_BATCH_LIMIT: i64 =
    query_result_cache_repository::MAX_QUERY_RESULT_CACHE_GC_BATCH_LIMIT;
pub(super) const QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT: i64 =
    query_result_cache_repository::MAX_QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT;
/// Hard per-tick work budget: at the configured 30-second cadence this drains
/// up to 5,000 expired keys (~166/s) while every statement remains a small
/// `SKIP LOCKED` batch.
pub(super) const QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK: u64 = 10;

struct QueryResultCacheGcMetrics {
    runs: Counter<u64>,
    batches: Counter<u64>,
    rows_deleted: Counter<u64>,
    budget_exhausted: Counter<u64>,
    backlog_probe_failures: Counter<u64>,
    backlog_sample_rows: Gauge<u64>,
    oldest_expired_age_seconds: Gauge<f64>,
}

fn query_result_cache_gc_metrics() -> &'static QueryResultCacheGcMetrics {
    static METRICS: OnceLock<QueryResultCacheGcMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("ironrag.maintenance");
        QueryResultCacheGcMetrics {
            runs: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.runs")
                .with_description("Completed PostgreSQL query-result cache GC sweeps")
                .with_unit("{run}")
                .build(),
            batches: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.batches")
                .with_description("Bounded PostgreSQL query-result cache GC statements")
                .with_unit("{batch}")
                .build(),
            rows_deleted: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.rows_deleted")
                .with_description("Expired PostgreSQL query-result cache rows deleted")
                .with_unit("{row}")
                .build(),
            budget_exhausted: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.budget_exhausted")
                .with_description("GC sweeps that consumed the complete per-tick batch budget")
                .with_unit("{run}")
                .build(),
            backlog_probe_failures: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.backlog_probe_failures")
                .with_description("Failed bounded post-sweep backlog probes")
                .with_unit("{probe}")
                .build(),
            backlog_sample_rows: meter
                .u64_gauge("ironrag.maintenance.query_result_cache_gc.backlog_sample_rows")
                .with_description(
                    "Expired rows remaining in the bounded post-sweep sample; capped at 501",
                )
                .with_unit("{row}")
                .build(),
            oldest_expired_age_seconds: meter
                .f64_gauge("ironrag.maintenance.query_result_cache_gc.oldest_expired_age_seconds")
                .with_description("Seconds the oldest sampled cache winner is past its TTL")
                .with_unit("s")
                .build(),
        }
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct QueryResultCacheGcSweepProgress {
    pub(super) rows_deleted: u64,
    batches: u64,
    last_batch_was_full: bool,
}

impl QueryResultCacheGcSweepProgress {
    pub(super) fn record_batch(&mut self, rows_deleted: u64) {
        self.rows_deleted = self.rows_deleted.saturating_add(rows_deleted);
        self.batches = self.batches.saturating_add(1);
        self.last_batch_was_full =
            rows_deleted == u64::try_from(QUERY_RESULT_CACHE_GC_BATCH_LIMIT).unwrap_or(u64::MAX);
    }

    #[must_use]
    pub(super) const fn budget_exhausted(self) -> bool {
        self.batches == QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK && self.last_batch_was_full
    }
}

pub(super) fn query_result_cache_gc_max_rows_per_tick() -> u64 {
    u64::try_from(QUERY_RESULT_CACHE_GC_BATCH_LIMIT)
        .unwrap_or(0)
        .saturating_mul(QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK)
}

/// Runs one bounded `PostgreSQL` result-cache GC batch.
///
/// The repository applies both the hard batch ceiling and the database-clock
/// TTL predicate. Exposing this one-shot surface keeps integration tests and
/// future operator tooling on the exact scheduler path.
pub async fn gc_expired_query_result_cache_once(
    state: &AppState,
    ttl: Duration,
    batch_limit: i64,
) -> anyhow::Result<u64> {
    Ok(query_result_cache_repository::delete_expired_query_result_cache_batch(
        &state.persistence.postgres,
        ttl.as_secs(),
        batch_limit,
    )
    .await?)
}

pub(super) async fn gc_expired_query_result_cache(state: &AppState) {
    let metrics = query_result_cache_gc_metrics();
    let ttl =
        Duration::from_secs(crate::services::query::result_cache::QUERY_RESULT_CACHE_TTL_SECONDS);
    let (progress, status) = run_query_result_cache_gc_batches(state, ttl).await;
    let budget_exhausted = progress.budget_exhausted();
    let backlog_probe = query_result_cache_repository::probe_expired_query_result_cache_backlog(
        &state.persistence.postgres,
        ttl.as_secs(),
        QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT,
    )
    .await;
    record_query_result_cache_backlog_probe(metrics, &backlog_probe, budget_exhausted, ttl);
    metrics.runs.add(1, &[KeyValue::new("status", status)]);
    metrics.batches.add(progress.batches, &[]);
    metrics.rows_deleted.add(progress.rows_deleted, &[]);
    if budget_exhausted {
        metrics.budget_exhausted.add(1, &[]);
    }
    log_query_result_cache_gc_completion(&progress, &backlog_probe, budget_exhausted, ttl);
}

/// Drains bounded batches until the budget is spent or a short batch ends the
/// walk, returning the accumulated progress and the metric status label.
async fn run_query_result_cache_gc_batches(
    state: &AppState,
    ttl: Duration,
) -> (QueryResultCacheGcSweepProgress, &'static str) {
    let mut progress = QueryResultCacheGcSweepProgress::default();
    let mut status = "success";
    for _ in 0..QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK {
        match gc_expired_query_result_cache_once(state, ttl, QUERY_RESULT_CACHE_GC_BATCH_LIMIT)
            .await
        {
            Ok(batch_rows) => {
                progress.record_batch(batch_rows);
                if !progress.last_batch_was_full {
                    break;
                }
            }
            Err(error) => {
                status = if progress.batches == 0 { "failed" } else { "partial_failure" };
                log_query_result_cache_gc_batch_failed(&error, &progress, ttl);
                break;
            }
        }
    }
    (progress, status)
}

fn record_query_result_cache_backlog_probe(
    metrics: &QueryResultCacheGcMetrics,
    backlog_probe: &Result<
        query_result_cache_repository::QueryResultCacheBacklogProbe,
        sqlx::Error,
    >,
    budget_exhausted: bool,
    ttl: Duration,
) {
    match backlog_probe {
        Ok(probe) => {
            let attributes = [
                KeyValue::new("has_backlog", probe.sampled_expired_rows > 0),
                KeyValue::new("sample_at_capacity", probe.sample_at_capacity()),
                KeyValue::new("budget_exhausted", budget_exhausted),
            ];
            metrics.backlog_sample_rows.record(probe.sampled_expired_rows, &attributes);
            metrics
                .oldest_expired_age_seconds
                .record(probe.oldest_expired_age_seconds.unwrap_or(0.0), &attributes);
        }
        Err(error) => {
            metrics.backlog_probe_failures.add(1, &[]);
            log_query_result_cache_backlog_probe_failed(error, ttl);
        }
    }
}

fn log_query_result_cache_gc_batch_failed(
    error: &anyhow::Error,
    progress: &QueryResultCacheGcSweepProgress,
    ttl: Duration,
) {
    warn!(
        ?error,
        completed_batches = progress.batches,
        rows_deleted = progress.rows_deleted,
        ttl_seconds = ttl.as_secs(),
        batch_limit = QUERY_RESULT_CACHE_GC_BATCH_LIMIT,
        "PostgreSQL query-result cache GC batch failed; ending this bounded sweep",
    );
}

fn log_query_result_cache_backlog_probe_failed(error: &sqlx::Error, ttl: Duration) {
    warn!(
        ?error,
        ttl_seconds = ttl.as_secs(),
        sample_limit = QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT,
        "bounded PostgreSQL query-result cache backlog probe failed",
    );
}

fn log_query_result_cache_gc_completion(
    progress: &QueryResultCacheGcSweepProgress,
    backlog_probe: &Result<
        query_result_cache_repository::QueryResultCacheBacklogProbe,
        sqlx::Error,
    >,
    budget_exhausted: bool,
    ttl: Duration,
) {
    let backlog_observed = backlog_probe.as_ref().is_ok_and(|probe| probe.sampled_expired_rows > 0);
    if progress.rows_deleted == 0 && !budget_exhausted && !backlog_observed {
        return;
    }
    let (backlog_sample_rows, backlog_sample_at_capacity, oldest_expired_age_seconds) =
        backlog_probe.as_ref().map_or((None, None, None), |probe| {
            (
                Some(probe.sampled_expired_rows),
                Some(probe.sample_at_capacity()),
                probe.oldest_expired_age_seconds,
            )
        });
    info!(
        rows_deleted = progress.rows_deleted,
        batches = progress.batches,
        budget_exhausted,
        ?backlog_sample_rows,
        ?backlog_sample_at_capacity,
        ?oldest_expired_age_seconds,
        ttl_seconds = ttl.as_secs(),
        batch_limit = QUERY_RESULT_CACHE_GC_BATCH_LIMIT,
        "maintenance scheduler completed PostgreSQL query-result cache GC sweep",
    );
}
