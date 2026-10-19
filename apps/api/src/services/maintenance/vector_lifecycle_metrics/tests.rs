//! Captured-OTel tests for the vector-lifecycle emitter.
//!
//! These assert on data points read back from a real SDK meter provider — the
//! instrument names, units, attribute keys and values that would actually be
//! exported — not on a mock method call. A metric that is recorded but never
//! observed here is a missing series, which is the defect the whole slice
//! exists to prevent.

use std::collections::BTreeSet;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, Metric, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use super::*;

/// Flattened data point: instrument name, unit, attributes and numeric value.
#[derive(Debug, Clone, PartialEq)]
struct Point {
    name: String,
    unit: String,
    attributes: Vec<(String, String)>,
    value: f64,
}

fn kv_pair(kv: &KeyValue) -> (String, String) {
    (kv.key.as_str().to_string(), kv.value.to_string())
}

fn append_points(metric: &Metric, points: &mut Vec<Point>) {
    match metric.data() {
        AggregatedMetrics::U64(MetricData::Sum(sum)) => {
            push_points(metric, sum.data_points(), points, |point| {
                (point.attributes().map(kv_pair).collect(), point.value() as f64)
            });
        }
        AggregatedMetrics::I64(MetricData::Sum(sum)) => {
            push_points(metric, sum.data_points(), points, |point| {
                (point.attributes().map(kv_pair).collect(), point.value() as f64)
            });
        }
        AggregatedMetrics::F64(MetricData::Sum(sum)) => {
            push_points(metric, sum.data_points(), points, |point| {
                (point.attributes().map(kv_pair).collect(), point.value())
            });
        }
        AggregatedMetrics::U64(MetricData::Gauge(gauge)) => {
            push_points(metric, gauge.data_points(), points, |point| {
                (point.attributes().map(kv_pair).collect(), point.value() as f64)
            });
        }
        AggregatedMetrics::I64(MetricData::Gauge(gauge)) => {
            push_points(metric, gauge.data_points(), points, |point| {
                (point.attributes().map(kv_pair).collect(), point.value() as f64)
            });
        }
        AggregatedMetrics::F64(MetricData::Gauge(gauge)) => {
            push_points(metric, gauge.data_points(), points, |point| {
                (point.attributes().map(kv_pair).collect(), point.value())
            });
        }
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
            push_points(metric, histogram.data_points(), points, |point| {
                (point.attributes().map(kv_pair).collect(), point.count() as f64)
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
        points.push(Point {
            name: metric.name().to_string(),
            unit: metric.unit().to_string(),
            attributes,
            value,
        });
    }
}

fn setup() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let reader = PeriodicReader::builder(exporter.clone()).build();
    let provider = SdkMeterProvider::builder().with_reader(reader).build();
    (provider, exporter)
}

fn collect(provider: &SdkMeterProvider, exporter: &InMemoryMetricExporter) -> Vec<Point> {
    provider.force_flush().expect("meter provider must flush");
    let mut points = Vec::new();
    for resource in exporter.get_finished_metrics().expect("finished metrics") {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics() {
                append_points(metric, &mut points);
            }
        }
    }
    points
}

fn point_named<'a>(points: &'a [Point], name: &str) -> Option<&'a Point> {
    points.iter().find(|point| point.name == name)
}

fn points_named<'a>(points: &'a [Point], name: &str) -> Vec<&'a Point> {
    points.iter().filter(|point| point.name == name).collect()
}

fn attribute<'a>(point: &'a Point, key: &str) -> Option<&'a str> {
    point
        .attributes
        .iter()
        .find(|(attribute_key, _)| attribute_key == key)
        .map(|(_, value)| value.as_str())
}

fn attribute_keys(point: &Point) -> BTreeSet<&str> {
    point.attributes.iter().map(|(key, _)| key.as_str()).collect()
}

fn success_observation() -> GcRunObservation {
    GcRunObservation {
        maintenance_class: vector_lifecycle_class(),
        outcome: GcRunOutcome::Success,
        skip_reason: None,
        error_code: None,
        candidate_documents: 7,
        deleted_superseded_vectors: 3,
        deleted_null_head_vectors: 2,
        duration: Duration::from_millis(250),
    }
}

fn inventory_snapshot() -> InventorySnapshot {
    InventorySnapshot {
        sampled_at_unix_seconds: 1_700_000_000.0,
        vector_inventory: vec![VectorInventoryObservation {
            vector_kind: VectorKind::Chunk,
            dimensions: 1536,
            lifecycle_category: LifecycleCategory::CurrentReadable,
            vector_rows: 42,
            logical_embedding_value_bytes: 65_536,
        }],
        physical_shards: vec![PhysicalShardObservation {
            vector_kind: VectorKind::Chunk,
            dimensions: 1536,
            physical_shard_bytes: 1_048_576,
        }],
        backlog: Some(GcBacklogObservation { vectors: 0, oldest_age_seconds: None }),
        gc_last_success_unix_seconds: Some(1_700_000_000.0),
    }
}

#[test]
fn every_contract_metric_is_recorded_with_the_declared_attribute_keys() {
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    instruments.record_gc_run(&success_observation());
    instruments.record_gc_run(&GcRunObservation {
        maintenance_class: vector_lifecycle_class(),
        outcome: GcRunOutcome::SkippedLibraryBusy,
        skip_reason: Some(GcSkipReason::LibraryBusy),
        error_code: None,
        candidate_documents: 0,
        deleted_superseded_vectors: 0,
        deleted_null_head_vectors: 0,
        duration: Duration::from_millis(5),
    });
    instruments.record_gc_run(&GcRunObservation {
        maintenance_class: vector_lifecycle_class(),
        outcome: GcRunOutcome::Failed,
        skip_reason: None,
        error_code: Some(GcErrorCode::VectorPlane),
        candidate_documents: 0,
        deleted_superseded_vectors: 0,
        deleted_null_head_vectors: 0,
        duration: Duration::from_millis(9),
    });
    instruments.record_inventory_snapshot(&inventory_snapshot());

    let observed = collect(&provider, &exporter);
    for definition in VECTOR_LIFECYCLE_METRICS {
        let metric_points = points_named(&observed, definition.name);
        assert!(!metric_points.is_empty(), "metric {} was never recorded", definition.name);
        let keys: BTreeSet<&str> =
            metric_points.iter().flat_map(|point| attribute_keys(point)).collect();
        let expected: BTreeSet<&str> = definition.attributes.iter().copied().collect();
        assert_eq!(keys, expected, "attribute keys drifted for {}", definition.name);
        assert!(
            metric_points.iter().all(|point| point.unit == definition.unit),
            "unit drifted for {}",
            definition.name,
        );
    }
}

#[test]
fn a_drained_backlog_overwrites_the_previous_stale_age() {
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    let mut stale = inventory_snapshot();
    stale.backlog = Some(GcBacklogObservation { vectors: 1, oldest_age_seconds: Some(90_000.0) });
    instruments.record_inventory_snapshot(&stale);
    instruments.record_inventory_snapshot(&inventory_snapshot());

    let observed = collect(&provider, &exporter);
    let backlog = point_named(&observed, METRIC_GC_BACKLOG_VECTORS)
        .expect("a zero backlog must still be emitted");
    assert_eq!(backlog.value, 0.0);
    let ages = points_named(&observed, METRIC_GC_BACKLOG_OLDEST_AGE_SECONDS);
    assert_eq!(ages.len(), 1, "a mutable state label must not retain an old backlog series");
    assert_eq!(ages[0].value, 0.0, "drained backlog must clear its former stale age");
}

#[test]
fn a_global_sample_reports_last_success_and_inventory_timestamp() {
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    instruments.record_inventory_snapshot(&inventory_snapshot());

    let observed = collect(&provider, &exporter);
    let inventory_success = point_named(&observed, METRIC_INVENTORY_LAST_SUCCESS_UNIX_SECONDS)
        .expect("inventory stamp");
    assert_eq!(inventory_success.value, 1_700_000_000.0);
    assert!(inventory_success.attributes.is_empty(), "the global stamp is label-free");
    let gc_success = point_named(&observed, METRIC_GC_LAST_SUCCESS_UNIX_SECONDS).expect("gc stamp");
    assert_eq!(gc_success.value, 1_700_000_000.0);
    assert_eq!(attribute(gc_success, LABEL_LIBRARY_ID), None, "no library identity on GC");
}

#[test]
fn a_gc_failure_is_emitted_as_a_bounded_error_without_raw_text() {
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    instruments.record_gc_run(&GcRunObservation {
        maintenance_class: vector_lifecycle_class(),
        outcome: GcRunOutcome::Failed,
        skip_reason: None,
        error_code: Some(GcErrorCode::CatalogLookup),
        candidate_documents: 0,
        deleted_superseded_vectors: 0,
        deleted_null_head_vectors: 0,
        duration: Duration::from_millis(4),
    });

    let observed = collect(&provider, &exporter);
    let errors = point_named(&observed, METRIC_GC_ERRORS).expect("error series");
    assert_eq!(attribute(errors, LABEL_ERROR_CODE), Some("catalog_lookup"));
    assert!(!attribute_keys(errors).iter().any(|key| key.contains("text")));
}

#[test]
fn a_failed_run_never_touches_success_deleted_or_last_success_series() {
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    instruments.record_gc_run(&GcRunObservation {
        maintenance_class: vector_lifecycle_class(),
        outcome: GcRunOutcome::Failed,
        skip_reason: None,
        error_code: Some(GcErrorCode::Postgres),
        candidate_documents: 99,
        deleted_superseded_vectors: 99,
        deleted_null_head_vectors: 99,
        duration: Duration::from_millis(9),
    });

    let observed = collect(&provider, &exporter);
    let runs = points_named(&observed, METRIC_GC_RUNS);
    assert_eq!(runs.len(), 1);
    assert_eq!(attribute(runs[0], LABEL_OUTCOME), Some("failed"));
    for name in [
        METRIC_GC_DELETED_VECTORS,
        METRIC_GC_CANDIDATE_DOCUMENTS,
        METRIC_GC_SKIPPED_RUNS,
        METRIC_GC_LAST_SUCCESS_UNIX_SECONDS,
    ] {
        assert!(points_named(&observed, name).is_empty(), "{name} must not advance on failure");
    }
}

#[test]
fn library_busy_is_a_skip_distinct_from_active_ingest() {
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    instruments.record_gc_run(&GcRunObservation {
        maintenance_class: vector_lifecycle_class(),
        outcome: GcRunOutcome::from_skip_reason(GcSkipReason::LibraryBusy),
        skip_reason: Some(GcSkipReason::LibraryBusy),
        error_code: None,
        candidate_documents: 0,
        deleted_superseded_vectors: 0,
        deleted_null_head_vectors: 0,
        duration: Duration::from_millis(3),
    });

    let observed = collect(&provider, &exporter);
    let skipped = points_named(&observed, METRIC_GC_SKIPPED_RUNS);
    assert!(
        skipped.iter().any(|point| attribute(point, LABEL_SKIP_REASON) == Some("library_busy"))
    );
    assert!(
        !skipped.iter().any(|point| attribute(point, LABEL_SKIP_REASON) == Some("active_ingest"))
    );
}

#[test]
fn physical_shard_bytes_cannot_carry_a_library_label() {
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    instruments.record_inventory_snapshot(&inventory_snapshot());

    let observed = collect(&provider, &exporter);
    let shard_points = points_named(&observed, METRIC_INVENTORY_PHYSICAL_SHARD_BYTES);
    assert!(!shard_points.is_empty());
    let expected: BTreeSet<&str> =
        [LABEL_VECTOR_KIND, LABEL_VECTOR_DIMENSIONS].into_iter().collect();
    for point in shard_points {
        assert_eq!(attribute_keys(point), expected);
    }
}

#[test]
fn inventory_series_have_no_library_label() {
    // IRG-68 regression: a sample for one library must not put the library
    // identity on any inventory or GC series. With thousands of libraries a
    // per-library label blows the 2000-series SDK cardinality ceiling and the
    // 24-hour freshness contract. Identity belongs in operator SQL and logs.
    let (provider, exporter) = setup();
    let instruments = VectorLifecycleInstruments::new(&provider.meter(METER_NAME));
    instruments.record_inventory_snapshot(&inventory_snapshot());

    let observed = collect(&provider, &exporter);
    let inventory_rows = points_named(&observed, METRIC_INVENTORY_VECTOR_ROWS);
    assert!(!inventory_rows.is_empty(), "the vector_rows series must be emitted");
    for point in &inventory_rows {
        assert!(
            !attribute_keys(point).contains(LABEL_LIBRARY_ID),
            "inventory.vector_rows must not carry {LABEL_LIBRARY_ID}: {:?}",
            point.attributes,
        );
    }
    let expected: BTreeSet<&str> =
        [LABEL_VECTOR_KIND, LABEL_VECTOR_DIMENSIONS, LABEL_LIFECYCLE_CATEGORY]
            .into_iter()
            .collect();
    for point in &inventory_rows {
        assert_eq!(attribute_keys(point), expected, "attribute keys drifted");
    }
    for name in [
        METRIC_INVENTORY_LOGICAL_EMBEDDING_VALUE_BYTES,
        METRIC_INVENTORY_LAST_SUCCESS_UNIX_SECONDS,
        METRIC_GC_BACKLOG_VECTORS,
        METRIC_GC_BACKLOG_OLDEST_AGE_SECONDS,
        METRIC_GC_LAST_SUCCESS_UNIX_SECONDS,
    ] {
        for point in points_named(&observed, name) {
            assert!(
                !attribute_keys(point).contains(LABEL_LIBRARY_ID),
                "{name} must not carry {LABEL_LIBRARY_ID}: {:?}",
                point.attributes,
            );
        }
    }
}

#[test]
fn every_label_helper_value_stays_inside_its_contract_domain() {
    assert!(contract_is_well_formed());
    let domain = |label: &str| -> BTreeSet<&'static str> {
        BOUNDED_LABEL_VALUE_DOMAINS
            .iter()
            .find(|candidate| candidate.label == label)
            .map(|candidate| candidate.values.iter().copied().collect())
            .expect("bounded label domain must exist")
    };
    let error_labels: BTreeSet<&str> = GcErrorCode::ALL.iter().map(|value| value.label()).collect();
    assert_eq!(error_labels, domain(LABEL_ERROR_CODE));
    assert!(
        GcSkipReason::ALL
            .iter()
            .map(|value| value.label())
            .all(|v| domain(LABEL_SKIP_REASON).contains(v))
    );
    let category_labels: BTreeSet<&str> = LIFECYCLE_CATEGORIES.iter().copied().collect();
    assert_eq!(
        category_labels,
        LifecycleCategory::ALL.map(LifecycleCategory::label).into_iter().collect::<BTreeSet<_>>()
    );
}

#[test]
fn instrument_name_constants_are_declared_in_the_contract() {
    let contract_names: BTreeSet<&str> =
        VECTOR_LIFECYCLE_METRICS.iter().map(|definition| definition.name).collect();
    for name in [
        METRIC_INVENTORY_VECTOR_ROWS,
        METRIC_INVENTORY_LOGICAL_EMBEDDING_VALUE_BYTES,
        METRIC_INVENTORY_PHYSICAL_SHARD_BYTES,
        METRIC_INVENTORY_LAST_SUCCESS_UNIX_SECONDS,
        METRIC_GC_RUNS,
        METRIC_GC_CANDIDATE_DOCUMENTS,
        METRIC_GC_DELETED_VECTORS,
        METRIC_GC_SKIPPED_RUNS,
        METRIC_GC_ERRORS,
        METRIC_GC_DURATION_SECONDS,
        METRIC_GC_BACKLOG_VECTORS,
        METRIC_GC_BACKLOG_OLDEST_AGE_SECONDS,
        METRIC_GC_LAST_SUCCESS_UNIX_SECONDS,
    ] {
        assert!(contract_names.contains(name), "instrument name {name} is not in the contract");
    }
}
