//! Canonical metric schema for vector-lifecycle observations.
//!
//! This module deliberately describes what a future collector and scheduler
//! hook must emit without creating an alternate lifecycle state model. The
//! durable maintenance lease remains the source of last-success state, while
//! the inventory remains the source of vector-category and physical-shard
//! facts.

/// Meter name shared with the existing maintenance instrumentation.
pub(crate) const METER_NAME: &str = "ironrag.maintenance";

pub(crate) const LABEL_LIBRARY_ID: &str = "ironrag.library.id";
pub(crate) const LABEL_VECTOR_KIND: &str = "ironrag.vector.kind";
pub(crate) const LABEL_VECTOR_DIMENSIONS: &str = "ironrag.vector.dimensions";
pub(crate) const LABEL_LIFECYCLE_CATEGORY: &str = "ironrag.vector.lifecycle.category";
pub(crate) const LABEL_MAINTENANCE_CLASS: &str = "ironrag.maintenance.class";
pub(crate) const LABEL_OUTCOME: &str = "outcome";
pub(crate) const LABEL_ERROR_CODE: &str = "error.code";
pub(crate) const LABEL_DELETION_REASON: &str = "deletion.reason";
pub(crate) const LABEL_SKIP_REASON: &str = "skip.reason";

/// These values deliberately mirror `scripts/ops/collect_vector_lifecycle_state.py`.
/// They apply only to chunk vectors: entity vectors have no revision-lifecycle
/// category and must not be squeezed into an invented `unknown` series.
pub(crate) const LIFECYCLE_CATEGORIES: &[&str] = &[
    "currentReadable",
    "protectedActiveIngest",
    "supersededReadableHistory",
    "inconsistentOrOrphan",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstrumentKind {
    Counter,
    Gauge,
    Histogram,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MetricDefinition {
    pub(crate) name: &'static str,
    pub(crate) unit: &'static str,
    pub(crate) instrument: InstrumentKind,
    pub(crate) attributes: &'static [&'static str],
}

/// A finite value domain for a metric label. Shard identifiers are intentionally
/// not listed here: a shard is `(kind, dimensions)` and carries no identity.
/// Every outcome, reason, and error-code label is listed here instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoundedLabelValueDomain {
    pub(crate) label: &'static str,
    pub(crate) values: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationScope {
    VectorLifecycle,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MaintenanceObservationContract {
    pub(crate) maintenance_class: &'static str,
    pub(crate) scope: ObservationScope,
}

/// The emitter selects vector-lifecycle observations by maintenance class;
/// other classes keep their own observation scope.
pub(crate) const MAINTENANCE_OBSERVATION_CONTRACTS: &[MaintenanceObservationContract] = &[
    MaintenanceObservationContract {
        maintenance_class: "gc.superseded-vectors",
        scope: ObservationScope::VectorLifecycle,
    },
    MaintenanceObservationContract {
        maintenance_class: "gc.runtime-graph",
        scope: ObservationScope::Other,
    },
    MaintenanceObservationContract {
        maintenance_class: "gc.historical-projection",
        scope: ObservationScope::Other,
    },
    MaintenanceObservationContract {
        maintenance_class: "graph.reextract-periodic-admission",
        scope: ObservationScope::Other,
    },
];

/// Every non-identity lifecycle label uses one of these finite domains. Future
/// scheduler wiring must map source outcomes to these values rather than pass
/// arbitrary strings through to the OpenTelemetry exporter.
pub(crate) const BOUNDED_LABEL_VALUE_DOMAINS: &[BoundedLabelValueDomain] = &[
    BoundedLabelValueDomain { label: LABEL_VECTOR_KIND, values: &["chunk", "entity"] },
    BoundedLabelValueDomain { label: LABEL_LIFECYCLE_CATEGORY, values: LIFECYCLE_CATEGORIES },
    BoundedLabelValueDomain {
        label: LABEL_OUTCOME,
        values: &["success", "skipped_active_ingest", "skipped_library_busy", "failed"],
    },
    BoundedLabelValueDomain {
        label: LABEL_ERROR_CODE,
        values: &[
            "catalog_lookup",
            "lease_complete",
            "lease_cursor",
            "lease_fail",
            "lease_requeue",
            "postgres",
            "runtime_graph",
            "unsupported_class_scope",
            "vector_plane",
        ],
    },
    BoundedLabelValueDomain {
        label: LABEL_DELETION_REASON,
        values: &["null_head", "superseded_readable_history"],
    },
    BoundedLabelValueDomain {
        label: LABEL_SKIP_REASON,
        values: &["active_ingest", "library_busy"],
    },
];

/// Cardinality and ownership contract for every emitted vector-lifecycle metric.
///
/// No metric carries `ironrag.library.id`. The inventory and GC series are
/// aggregated globally because a per-library label grows with the catalog and
/// overflows the SDK's 2000-series ceiling; a per-library drill-down is operator
/// SQL and structured logs. `logical_embedding_value_bytes` is the sum of
/// individual embedding datum sizes for one shard and category, not a table or
/// index storage estimate. `physical_shard_bytes` is the exact shared relation
/// size and therefore omits the lifecycle-category label.
pub(crate) const VECTOR_LIFECYCLE_METRICS: &[MetricDefinition] = &[
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.inventory.vector_rows",
        unit: "{vector}",
        instrument: InstrumentKind::Gauge,
        attributes: &[LABEL_VECTOR_KIND, LABEL_VECTOR_DIMENSIONS, LABEL_LIFECYCLE_CATEGORY],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.inventory.logical_embedding_value_bytes",
        unit: "By",
        instrument: InstrumentKind::Gauge,
        attributes: &[LABEL_VECTOR_KIND, LABEL_VECTOR_DIMENSIONS, LABEL_LIFECYCLE_CATEGORY],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.inventory.physical_shard_bytes",
        unit: "By",
        instrument: InstrumentKind::Gauge,
        attributes: &[LABEL_VECTOR_KIND, LABEL_VECTOR_DIMENSIONS],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.inventory.last_success_unix_seconds",
        unit: "s",
        instrument: InstrumentKind::Gauge,
        attributes: &[],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.runs",
        unit: "{run}",
        instrument: InstrumentKind::Counter,
        attributes: &[LABEL_MAINTENANCE_CLASS, LABEL_OUTCOME],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.candidate_documents",
        unit: "{document}",
        instrument: InstrumentKind::Counter,
        attributes: &[LABEL_MAINTENANCE_CLASS],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.deleted_vectors",
        unit: "{vector}",
        instrument: InstrumentKind::Counter,
        attributes: &[LABEL_MAINTENANCE_CLASS, LABEL_DELETION_REASON],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.skipped_runs",
        unit: "{run}",
        instrument: InstrumentKind::Counter,
        attributes: &[LABEL_MAINTENANCE_CLASS, LABEL_SKIP_REASON],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.errors",
        unit: "{error}",
        instrument: InstrumentKind::Counter,
        attributes: &[LABEL_MAINTENANCE_CLASS, LABEL_ERROR_CODE],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.duration_seconds",
        unit: "s",
        instrument: InstrumentKind::Histogram,
        attributes: &[LABEL_MAINTENANCE_CLASS, LABEL_OUTCOME],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.backlog_vectors",
        unit: "{vector}",
        instrument: InstrumentKind::Gauge,
        attributes: &[LABEL_MAINTENANCE_CLASS],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.backlog_oldest_age_seconds",
        unit: "s",
        instrument: InstrumentKind::Gauge,
        attributes: &[LABEL_MAINTENANCE_CLASS],
    },
    MetricDefinition {
        name: "ironrag.maintenance.vector_lifecycle.gc.last_success_unix_seconds",
        unit: "s",
        instrument: InstrumentKind::Gauge,
        attributes: &[LABEL_MAINTENANCE_CLASS],
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn metric(name: &str) -> &'static MetricDefinition {
        VECTOR_LIFECYCLE_METRICS
            .iter()
            .find(|definition| definition.name == name)
            .expect("metric definition must exist")
    }

    fn domain_values(label: &str) -> &'static [&'static str] {
        BOUNDED_LABEL_VALUE_DOMAINS
            .iter()
            .find(|domain| domain.label == label)
            .map(|domain| domain.values)
            .expect("bounded label domain must exist")
    }

    #[test]
    fn metrics_and_maintenance_contracts_have_unique_names() {
        let metric_names = VECTOR_LIFECYCLE_METRICS
            .iter()
            .map(|definition| definition.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(metric_names.len(), VECTOR_LIFECYCLE_METRICS.len());

        let classes = MAINTENANCE_OBSERVATION_CONTRACTS
            .iter()
            .map(|contract| contract.maintenance_class)
            .collect::<BTreeSet<_>>();
        assert_eq!(classes.len(), MAINTENANCE_OBSERVATION_CONTRACTS.len());
    }

    #[test]
    fn logical_metrics_keep_the_canonical_lifecycle_categories() {
        assert_eq!(
            LIFECYCLE_CATEGORIES,
            [
                "currentReadable",
                "protectedActiveIngest",
                "supersededReadableHistory",
                "inconsistentOrOrphan",
            ]
        );
        for name in [
            "ironrag.maintenance.vector_lifecycle.inventory.vector_rows",
            "ironrag.maintenance.vector_lifecycle.inventory.logical_embedding_value_bytes",
        ] {
            let attributes = metric(name).attributes;
            assert!(!attributes.contains(&LABEL_LIBRARY_ID), "{name} must be aggregated globally");
            assert!(attributes.contains(&LABEL_VECTOR_KIND), "{name}");
            assert!(attributes.contains(&LABEL_VECTOR_DIMENSIONS), "{name}");
            assert!(attributes.contains(&LABEL_LIFECYCLE_CATEGORY), "{name}");
        }
    }

    #[test]
    fn the_global_inventory_last_success_is_label_free() {
        let attributes =
            metric("ironrag.maintenance.vector_lifecycle.inventory.last_success_unix_seconds")
                .attributes;
        assert!(attributes.is_empty(), "the global inventory stamp carries no identity label");
    }

    #[test]
    fn no_vector_lifecycle_metric_declares_the_library_label() {
        for definition in VECTOR_LIFECYCLE_METRICS {
            assert!(
                !definition.attributes.contains(&LABEL_LIBRARY_ID),
                "{} must not declare {LABEL_LIBRARY_ID}",
                definition.name,
            );
        }
    }

    #[test]
    fn physical_shard_bytes_cannot_be_counted_per_library() {
        let attributes =
            metric("ironrag.maintenance.vector_lifecycle.inventory.physical_shard_bytes")
                .attributes;
        assert_eq!(attributes, [LABEL_VECTOR_KIND, LABEL_VECTOR_DIMENSIONS]);
        assert!(!attributes.contains(&LABEL_LIBRARY_ID));
        assert!(!attributes.contains(&LABEL_LIFECYCLE_CATEGORY));
    }

    #[test]
    fn outcome_reason_and_error_labels_have_finite_value_domains() {
        let labels =
            BOUNDED_LABEL_VALUE_DOMAINS.iter().map(|domain| domain.label).collect::<BTreeSet<_>>();
        assert_eq!(labels.len(), BOUNDED_LABEL_VALUE_DOMAINS.len());
        assert_eq!(
            domain_values(LABEL_OUTCOME),
            &["success", "skipped_active_ingest", "skipped_library_busy", "failed",][..]
        );
        assert_eq!(domain_values(LABEL_SKIP_REASON), &["active_ingest", "library_busy"][..]);
        assert_eq!(
            domain_values(LABEL_DELETION_REASON),
            &["null_head", "superseded_readable_history"][..]
        );
        assert!(domain_values(LABEL_ERROR_CODE).contains(&"catalog_lookup"));
        assert!(domain_values(LABEL_ERROR_CODE).contains(&"vector_plane"));
        assert!(domain_values(LABEL_ERROR_CODE).iter().all(|value| !value.contains(' ')));
    }

    #[test]
    fn gc_error_metric_keeps_error_text_out_of_metric_labels() {
        let attributes = metric("ironrag.maintenance.vector_lifecycle.gc.errors").attributes;
        assert!(attributes.contains(&LABEL_ERROR_CODE));
        assert!(!attributes.iter().any(|attribute| attribute.contains("text")));
    }
}
