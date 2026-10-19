//! The single owner of IronRAG's operational metric definitions (IRG-204).
//!
//! Every new counter, histogram and gauge is declared here and nowhere else, so
//! the exported names, units and label sets live in one reviewable place. Call
//! sites use the free `record_*` functions, which resolve the process-global
//! meter once.
//!
//! Label discipline: only bounded, low-cardinality values are used. Identifiers
//! (workspace/library/document/attempt/provider-call UUIDs) are never labels;
//! free-form codes pass through `sanitize_label` so no unbounded token can
//! explode the time series.

use std::sync::OnceLock;

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Gauge, Histogram, Meter},
};

/// Outcome of one terminal ingest attempt, as recorded by the finalizers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    /// The attempt committed its work.
    Succeeded,
    /// The attempt failed and exhausted its retry budget.
    Failed,
    /// The attempt was canceled (operator, deleted document, worker shutdown).
    Canceled,
    /// The attempt failed retryably and its job was pushed back to the queue.
    Requeued,
}

impl IngestOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Requeued => "requeued",
        }
    }
}

/// Which stale-lease reaper performed the recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseReaper {
    /// The in-runtime tokio recovery loop.
    Tokio,
    /// The dedicated OS-thread reaper that survives a saturated runtime.
    External,
}

impl LeaseReaper {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tokio => "tokio",
            Self::External => "external",
        }
    }
}

/// What a stale-lease recovery did to a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseRecoveryKind {
    /// The stale job was put back to `queued` for another worker.
    Requeued,
    /// The job was already terminal; only its orphaned leased attempt was failed.
    OrphanedAttempt,
}

impl LeaseRecoveryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requeued => "requeued",
            Self::OrphanedAttempt => "orphaned_attempt",
        }
    }
}

/// Semantic-cache lookup result for graph extraction reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphCacheResult {
    /// A ready extraction record was reused.
    Hit,
    /// No usable record existed and the provider was called.
    Miss,
}

impl GraphCacheResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

/// How a tracked provider call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderCallStatus {
    /// A durable reservation was created before the request left the process.
    Started,
    /// The provider returned a response whose usage was recorded.
    Completed,
    /// The call ended without provider usage.
    Failed,
    /// The call was canceled before a response.
    Canceled,
    /// The provider answered with HTTP 429 (rate limited / quota exhausted).
    RateLimited,
}

impl ProviderCallStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::RateLimited => "rate_limited",
        }
    }
}

/// Age bucket of the oldest non-terminal row in an aggregate sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgeBucket {
    Under5Minutes,
    Under1Hour,
    Under24Hours,
    AtLeast24Hours,
}

impl AgeBucket {
    /// Every bucket, in export order. One source for the bounded cross product
    /// the non-terminal sampler expands so a missing group reports `0`.
    pub const ALL: [AgeBucket; 4] =
        [Self::Under5Minutes, Self::Under1Hour, Self::Under24Hours, Self::AtLeast24Hours];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Under5Minutes => "<5m",
            Self::Under1Hour => "<1h",
            Self::Under24Hours => "<24h",
            Self::AtLeast24Hours => ">=24h",
        }
    }
}

/// The five bounded ingest job kinds. Anything else is reported as `other` so a
/// new kind cannot silently widen the label set.
#[must_use]
pub fn normalize_job_kind(job_kind: &str) -> &'static str {
    match job_kind {
        "content_mutation" => "content_mutation",
        "graph_refresh" => "graph_refresh",
        "web_discovery" => "web_discovery",
        "web_materialize_page" => "web_materialize_page",
        "webhook_delivery" => "webhook_delivery",
        _ => "other",
    }
}

/// Classifies a finalized attempt from its persisted terminal state and the job
/// state the finalizer derived. A retryable failure leaves the job `queued`.
#[must_use]
pub fn classify_ingest_outcome(attempt_state: &str, next_queue_state: &str) -> IngestOutcome {
    match (attempt_state, next_queue_state) {
        ("succeeded", _) => IngestOutcome::Succeeded,
        ("canceled", _) => IngestOutcome::Canceled,
        (_, "queued") => IngestOutcome::Requeued,
        _ => IngestOutcome::Failed,
    }
}

/// Converts a bounded attempt start/finish pair into non-negative seconds.
#[must_use]
pub fn elapsed_seconds(
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> f64 {
    let millis = (end - start).num_milliseconds();
    if millis <= 0 { 0.0 } else { millis as f64 / 1_000.0 }
}

fn sanitize_label(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len().min(96));
    for character in value.chars().take(96) {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '/' | ':') {
            sanitized.push(character);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        sanitized.push_str("unknown");
    }
    sanitized
}

fn failure_code_label(failure_code: Option<&str>) -> String {
    failure_code.map_or_else(|| "none".to_string(), sanitize_label)
}

struct Metrics {
    ingest_attempts: Counter<u64>,
    ingest_attempt_duration: Histogram<f64>,
    ingest_lease_recoveries: Counter<u64>,
    reap_stale_leases: Counter<u64>,
    reap_stale_query_executions: Counter<u64>,
    nonterminal_records: Gauge<u64>,
    graph_extraction_cache: Counter<u64>,
    provider_calls: Counter<u64>,
    provider_cost_usd: Counter<f64>,
    semantic_rerank_degradations: Counter<u64>,
}

impl Metrics {
    fn new(
        ingest_meter: &Meter,
        maintenance_meter: &Meter,
        graph_meter: &Meter,
        provider_meter: &Meter,
        retrieval_meter: &Meter,
    ) -> Self {
        Self {
            ingest_attempts: ingest_meter
                .u64_counter("ironrag.ingest.attempts")
                .with_description("Terminal outcome of an ingest attempt")
                .with_unit("{attempt}")
                .build(),
            ingest_attempt_duration: ingest_meter
                .f64_histogram("ironrag.ingest.attempt_duration")
                .with_description("Wall-clock duration of an ingest attempt")
                .with_unit("s")
                .build(),
            ingest_lease_recoveries: ingest_meter
                .u64_counter("ironrag.ingest.lease_recoveries")
                .with_description("Stale ingest leases recovered by the runtime or external reaper")
                .with_unit("{lease}")
                .build(),
            reap_stale_leases: maintenance_meter
                .u64_counter("ironrag.maintenance.reap_stale_leases")
                .with_description("Stale maintenance job leases reaped by the scheduler")
                .with_unit("{lease}")
                .build(),
            reap_stale_query_executions: maintenance_meter
                .u64_counter("ironrag.maintenance.reap_stale_query_executions")
                .with_description("Stale query executions canceled by the scheduler")
                .with_unit("{execution}")
                .build(),
            nonterminal_records: maintenance_meter
                .u64_gauge("ironrag.nonterminal_records")
                .with_description("Non-terminal records sampled by table, state and age bucket")
                .with_unit("{record}")
                .build(),
            graph_extraction_cache: graph_meter
                .u64_counter("ironrag.graph_extraction.cache")
                .with_description("Graph extraction semantic-cache lookup result")
                .with_unit("{lookup}")
                .build(),
            provider_calls: provider_meter
                .u64_counter("ironrag.provider.calls")
                .with_description("Tracked provider calls by kind, model and terminal status")
                .with_unit("{call}")
                .build(),
            provider_cost_usd: provider_meter
                .f64_counter("ironrag.provider.cost_usd")
                .with_description("Provider cost recorded by tracked calls, in USD")
                .with_unit("USD")
                .build(),
            semantic_rerank_degradations: retrieval_meter
                .u64_counter("ironrag.semantic_rerank.degradations")
                .with_description(
                    "Optional active semantic rerank fell back to lexical order, by reason",
                )
                .with_unit("{degradation}")
                .build(),
        }
    }

    fn record_ingest_attempt(
        &self,
        job_kind: &str,
        outcome: IngestOutcome,
        failure_code: Option<&str>,
        duration_seconds: f64,
    ) {
        let job_kind = normalize_job_kind(job_kind);
        let outcome = outcome.as_str();
        self.ingest_attempts.add(
            1,
            &[
                KeyValue::new("job_kind", job_kind),
                KeyValue::new("outcome", outcome),
                KeyValue::new("failure_code", failure_code_label(failure_code)),
            ],
        );
        self.ingest_attempt_duration.record(
            duration_seconds.max(0.0),
            &[KeyValue::new("job_kind", job_kind), KeyValue::new("outcome", outcome)],
        );
    }

    fn record_ingest_attempt_outcome(
        &self,
        job_kind: &str,
        outcome: IngestOutcome,
        failure_code: Option<&str>,
    ) {
        self.ingest_attempts.add(
            1,
            &[
                KeyValue::new("job_kind", normalize_job_kind(job_kind)),
                KeyValue::new("outcome", outcome.as_str()),
                KeyValue::new("failure_code", failure_code_label(failure_code)),
            ],
        );
    }

    fn record_lease_recovery(&self, reaper: LeaseReaper, kind: LeaseRecoveryKind, count: u64) {
        if count == 0 {
            return;
        }
        self.ingest_lease_recoveries.add(
            count,
            &[KeyValue::new("reaper", reaper.as_str()), KeyValue::new("kind", kind.as_str())],
        );
    }

    fn record_reap_stale_leases(&self, reaped: u64) {
        if reaped == 0 {
            return;
        }
        self.reap_stale_leases.add(reaped, &[]);
    }

    fn record_reap_stale_query_executions(&self, reaped: u64) {
        if reaped == 0 {
            return;
        }
        self.reap_stale_query_executions.add(reaped, &[]);
    }

    fn record_nonterminal_records(
        &self,
        table: &str,
        state: &str,
        age_bucket: AgeBucket,
        count: u64,
    ) {
        self.nonterminal_records.record(
            count,
            &[
                KeyValue::new("table", sanitize_label(table)),
                KeyValue::new("state", sanitize_label(state)),
                KeyValue::new("age_bucket", age_bucket.as_str()),
            ],
        );
    }

    fn record_graph_extraction_cache(&self, result: GraphCacheResult) {
        self.graph_extraction_cache.add(1, &[KeyValue::new("result", result.as_str())]);
    }

    fn record_provider_call(&self, call_kind: &str, model: &str, status: ProviderCallStatus) {
        self.provider_calls.add(
            1,
            &[
                KeyValue::new("call_kind", sanitize_label(call_kind)),
                KeyValue::new("model", sanitize_label(model)),
                KeyValue::new("status", status.as_str()),
            ],
        );
    }

    fn record_provider_cost_usd(&self, call_kind: &str, model: &str, usd: f64) {
        if !usd.is_finite() || usd <= 0.0 {
            return;
        }
        self.provider_cost_usd.add(
            usd,
            &[
                KeyValue::new("call_kind", sanitize_label(call_kind)),
                KeyValue::new("model", sanitize_label(model)),
            ],
        );
    }

    fn record_semantic_rerank_degradation(&self, reason: &str) {
        self.semantic_rerank_degradations
            .add(1, &[KeyValue::new("reason", sanitize_label(reason))]);
    }
}

fn metrics() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        Metrics::new(
            &global::meter("ironrag.ingest"),
            &global::meter("ironrag.maintenance"),
            &global::meter("ironrag.graph"),
            &global::meter("ironrag.provider"),
            &global::meter("ironrag.retrieval"),
        )
    })
}

/// Records one terminal ingest attempt and its duration in seconds.
pub fn record_ingest_attempt(
    job_kind: &str,
    outcome: IngestOutcome,
    failure_code: Option<&str>,
    duration_seconds: f64,
) {
    metrics().record_ingest_attempt(job_kind, outcome, failure_code, duration_seconds);
}

/// Records one terminal ingest attempt when its start time is not in scope.
///
/// Used by failure finalizers that finalize directly in the repository without
/// loading the attempt row; [`record_ingest_attempt`] is preferred when the
/// duration is available.
pub fn record_ingest_attempt_outcome(
    job_kind: &str,
    outcome: IngestOutcome,
    failure_code: Option<&str>,
) {
    metrics().record_ingest_attempt_outcome(job_kind, outcome, failure_code);
}

/// Records how many stale ingest leases one reaper recovered, split by kind.
pub fn record_lease_recovery(reaper: LeaseReaper, kind: LeaseRecoveryKind, count: u64) {
    metrics().record_lease_recovery(reaper, kind, count);
}

/// Records how many stale maintenance job leases the scheduler reaped.
pub fn record_reap_stale_leases(reaped: u64) {
    metrics().record_reap_stale_leases(reaped);
}

/// Records how many stale query executions the scheduler canceled.
pub fn record_reap_stale_query_executions(reaped: u64) {
    metrics().record_reap_stale_query_executions(reaped);
}

/// Records the current count of non-terminal records for one table/state/bucket.
pub fn record_nonterminal_records(table: &str, state: &str, age_bucket: AgeBucket, count: u64) {
    metrics().record_nonterminal_records(table, state, age_bucket, count);
}

/// Records one graph-extraction semantic-cache lookup result.
pub fn record_graph_extraction_cache(result: GraphCacheResult) {
    metrics().record_graph_extraction_cache(result);
}

/// Records one tracked provider call.
pub fn record_provider_call(call_kind: &str, model: &str, status: ProviderCallStatus) {
    metrics().record_provider_call(call_kind, model, status);
}

/// Records provider cost in USD for one tracked call.
pub fn record_provider_cost_usd(call_kind: &str, model: &str, usd: f64) {
    metrics().record_provider_cost_usd(call_kind, model, usd);
}

/// Records one fallback of the optional active semantic rerank, by bounded
/// reason. A permanently degraded binding is visible as a growing series even
/// when no single log line is inspected.
pub fn record_semantic_rerank_degradation(reason: &str) {
    metrics().record_semantic_rerank_degradation(reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::Value;
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, Metric, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    /// Flattened data point: instrument name, attributes and numeric value.
    struct Point {
        name: String,
        attributes: Vec<(String, String)>,
        value: f64,
    }

    fn kv_pair(kv: &KeyValue) -> (String, String) {
        let value = match &kv.value {
            Value::String(value) => value.as_str().to_string(),
            other => format!("{other:?}"),
        };
        (kv.key.as_str().to_string(), value)
    }

    fn collect(provider: &SdkMeterProvider, exporter: &InMemoryMetricExporter) -> Vec<Point> {
        provider.force_flush().unwrap();
        let finished = exporter.get_finished_metrics().unwrap();
        let mut points = Vec::new();
        for resource in &finished {
            for scope in resource.scope_metrics() {
                for metric in scope.metrics() {
                    append_metric_points(metric, &mut points);
                }
            }
        }
        points
    }

    fn append_metric_points(metric: &Metric, points: &mut Vec<Point>) {
        match metric.data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                push_points(metric, sum.data_points(), points, |point| {
                    (point.attributes().map(kv_pair).collect(), point.value() as f64)
                });
            }
            AggregatedMetrics::F64(MetricData::Sum(sum)) => {
                push_points(metric, sum.data_points(), points, |point| {
                    (point.attributes().map(kv_pair).collect(), point.value())
                });
            }
            AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                push_points(metric, histogram.data_points(), points, |point| {
                    (point.attributes().map(kv_pair).collect(), point.count() as f64)
                });
            }
            AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
                push_points(metric, gauge.data_points(), points, |point| {
                    (point.attributes().map(kv_pair).collect(), point.value() as f64)
                });
            }
            _ => {}
        }
    }

    fn push_points<'a, T: 'a>(
        metric: &Metric,
        data_points: impl Iterator<Item = &'a T>,
        points: &mut Vec<Point>,
        project: impl Fn(&T) -> (Vec<(String, String)>, f64),
    ) {
        for point in data_points {
            let (attributes, value) = project(point);
            points.push(Point { name: metric.name().to_string(), attributes, value });
        }
    }

    fn test_fixture() -> (SdkMeterProvider, InMemoryMetricExporter, Metrics) {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let meter = provider.meter("ironrag.metrics.test");
        let metrics = Metrics::new(&meter, &meter, &meter, &meter, &meter);
        (provider, exporter, metrics)
    }

    fn find<'a>(points: &'a [Point], name: &str, key: &str, value: &str) -> &'a Point {
        points
            .iter()
            .find(|point| {
                point.name == name && point.attributes.iter().any(|(k, v)| k == key && v == value)
            })
            .unwrap_or_else(|| panic!("no {name} data point with {key}={value}"))
    }

    #[test]
    fn ingest_attempt_counter_separates_outcome_and_failure_code() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_ingest_attempt(
            "content_mutation",
            IngestOutcome::Failed,
            Some("canonical_pipeline_failed"),
            2.5,
        );
        let points = collect(&provider, &exporter);
        let point = find(&points, "ironrag.ingest.attempts", "outcome", "failed");
        assert_eq!(point.value, 1.0);
        assert_eq!(
            point.attributes.iter().find(|(k, _)| k == "job_kind").map(|(_, v)| v.as_str()),
            Some("content_mutation")
        );
        assert_eq!(
            point.attributes.iter().find(|(k, _)| k == "failure_code").map(|(_, v)| v.as_str()),
            Some("canonical_pipeline_failed")
        );
    }

    #[test]
    fn ingest_attempt_duration_is_recorded_as_seconds() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_ingest_attempt("graph_refresh", IngestOutcome::Succeeded, None, 3.0);
        let points = collect(&provider, &exporter);
        let point = find(&points, "ironrag.ingest.attempt_duration", "outcome", "succeeded");
        assert_eq!(point.value, 1.0, "one histogram observation");
        assert_eq!(
            point.attributes.iter().find(|(k, _)| k == "job_kind").map(|(_, v)| v.as_str()),
            Some("graph_refresh")
        );
    }

    #[test]
    fn lease_recoveries_split_reaper_and_kind() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_lease_recovery(LeaseReaper::External, LeaseRecoveryKind::OrphanedAttempt, 2);
        metrics.record_lease_recovery(LeaseReaper::Tokio, LeaseRecoveryKind::Requeued, 1);
        let points = collect(&provider, &exporter);
        assert_eq!(
            find(&points, "ironrag.ingest.lease_recoveries", "reaper", "external").value,
            2.0
        );
        assert_eq!(find(&points, "ironrag.ingest.lease_recoveries", "kind", "requeued").value, 1.0);
        assert_eq!(find(&points, "ironrag.ingest.lease_recoveries", "reaper", "tokio").value, 1.0);
    }

    #[test]
    fn reaper_counters_are_recorded() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_reap_stale_leases(4);
        metrics.record_reap_stale_query_executions(3);
        let points = collect(&provider, &exporter);
        assert_eq!(
            points
                .iter()
                .find(|p| p.name == "ironrag.maintenance.reap_stale_leases")
                .map(|p| p.value),
            Some(4.0)
        );
        assert_eq!(
            points
                .iter()
                .find(|p| p.name == "ironrag.maintenance.reap_stale_query_executions")
                .map(|p| p.value),
            Some(3.0)
        );
    }

    #[test]
    fn nonterminal_gauge_carries_bounded_labels() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_nonterminal_records("ingest_job", "queued", AgeBucket::AtLeast24Hours, 7);
        let points = collect(&provider, &exporter);
        let point = find(&points, "ironrag.nonterminal_records", "table", "ingest_job");
        assert_eq!(point.value, 7.0);
        assert_eq!(
            point.attributes.iter().find(|(k, _)| k == "age_bucket").map(|(_, v)| v.as_str()),
            Some(">=24h")
        );
    }

    #[test]
    fn graph_cache_counter_separates_hit_and_miss() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_graph_extraction_cache(GraphCacheResult::Hit);
        metrics.record_graph_extraction_cache(GraphCacheResult::Miss);
        let points = collect(&provider, &exporter);
        assert_eq!(find(&points, "ironrag.graph_extraction.cache", "result", "hit").value, 1.0);
        assert_eq!(find(&points, "ironrag.graph_extraction.cache", "result", "miss").value, 1.0);
    }

    #[test]
    fn provider_calls_and_cost_carry_kind_and_model() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_provider_call("extract_graph", "gpt-4o", ProviderCallStatus::RateLimited);
        metrics.record_provider_cost_usd("extract_graph", "gpt-4o", 0.25);
        let points = collect(&provider, &exporter);
        let call = find(&points, "ironrag.provider.calls", "status", "rate_limited");
        assert_eq!(call.value, 1.0);
        assert_eq!(
            call.attributes.iter().find(|(k, _)| k == "model").map(|(_, v)| v.as_str()),
            Some("gpt-4o")
        );
        let cost = find(&points, "ironrag.provider.cost_usd", "call_kind", "extract_graph");
        assert!((cost.value - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn semantic_rerank_degradations_carry_a_bounded_reason() {
        let (provider, exporter, metrics) = test_fixture();
        metrics.record_semantic_rerank_degradation("missing_binding");
        metrics.record_semantic_rerank_degradation("provider_budget_exceeds_turn_wait");
        let points = collect(&provider, &exporter);
        assert_eq!(
            find(&points, "ironrag.semantic_rerank.degradations", "reason", "missing_binding")
                .value,
            1.0
        );
        assert_eq!(
            find(
                &points,
                "ironrag.semantic_rerank.degradations",
                "reason",
                "provider_budget_exceeds_turn_wait"
            )
            .value,
            1.0
        );
    }

    #[test]
    fn unknown_job_kind_is_bounded_to_other() {
        assert_eq!(normalize_job_kind("brand_new_kind"), "other");
        assert_eq!(normalize_job_kind("content_mutation"), "content_mutation");
    }

    #[test]
    fn sanitize_label_replaces_unbounded_characters() {
        assert_eq!(sanitize_label("a b\tc"), "a_b_c");
        assert_eq!(sanitize_label(""), "unknown");
    }
}
