//! Emitters for the vector-lifecycle observation contract.
//!
//! [`super::vector_lifecycle_observability`] declares the metric names, units,
//! attribute keys and bounded label domains; it deliberately emits nothing.
//! This module is the emit point: it turns one completed scheduler dispatch or
//! one bounded global inventory probe into exactly the series that contract
//! declares, using only label values from the finite domains.
//!
//! Instruments are built from a caller-supplied [`Meter`] so tests can inject a
//! private meter provider; production reaches the process-wide provider through
//! `VectorLifecycleInstruments::global`. The bounded probe
//! (`super::vector_lifecycle_inventory::sample_global_inventory`) reads the canonical physical-shard inventory rather
//! than inventing a second lifecycle state model: it reuses the relation
//! discovery and the current-vector predicate the sweeps already trust.

use std::{
    collections::BTreeSet,
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Gauge, Histogram, Meter},
};

use crate::services::{
    graph::gc as graph_gc,
    maintenance::{
        gc::GcError,
        lease::MaintenanceClass,
        vector_lifecycle_observability::{
            BOUNDED_LABEL_VALUE_DOMAINS, InstrumentKind, LABEL_DELETION_REASON, LABEL_ERROR_CODE,
            LABEL_LIBRARY_ID, LABEL_LIFECYCLE_CATEGORY, LABEL_MAINTENANCE_CLASS, LABEL_OUTCOME,
            LABEL_SKIP_REASON, LABEL_VECTOR_DIMENSIONS, LABEL_VECTOR_KIND, LIFECYCLE_CATEGORIES,
            MAINTENANCE_OBSERVATION_CONTRACTS, METER_NAME, MetricDefinition, ObservationScope,
            VECTOR_LIFECYCLE_METRICS,
        },
    },
};

/// Instrument names, kept as constants so the contract-binding tests can name
/// the exact string a builder call uses.
pub(crate) const METRIC_INVENTORY_VECTOR_ROWS: &str =
    "ironrag.maintenance.vector_lifecycle.inventory.vector_rows";
pub(crate) const METRIC_INVENTORY_LOGICAL_EMBEDDING_VALUE_BYTES: &str =
    "ironrag.maintenance.vector_lifecycle.inventory.logical_embedding_value_bytes";
pub(crate) const METRIC_INVENTORY_PHYSICAL_SHARD_BYTES: &str =
    "ironrag.maintenance.vector_lifecycle.inventory.physical_shard_bytes";
pub(crate) const METRIC_INVENTORY_LAST_SUCCESS_UNIX_SECONDS: &str =
    "ironrag.maintenance.vector_lifecycle.inventory.last_success_unix_seconds";
pub(crate) const METRIC_GC_RUNS: &str = "ironrag.maintenance.vector_lifecycle.gc.runs";
pub(crate) const METRIC_GC_CANDIDATE_DOCUMENTS: &str =
    "ironrag.maintenance.vector_lifecycle.gc.candidate_documents";
pub(crate) const METRIC_GC_DELETED_VECTORS: &str =
    "ironrag.maintenance.vector_lifecycle.gc.deleted_vectors";
pub(crate) const METRIC_GC_SKIPPED_RUNS: &str =
    "ironrag.maintenance.vector_lifecycle.gc.skipped_runs";
pub(crate) const METRIC_GC_ERRORS: &str = "ironrag.maintenance.vector_lifecycle.gc.errors";
pub(crate) const METRIC_GC_DURATION_SECONDS: &str =
    "ironrag.maintenance.vector_lifecycle.gc.duration_seconds";
pub(crate) const METRIC_GC_BACKLOG_VECTORS: &str =
    "ironrag.maintenance.vector_lifecycle.gc.backlog_vectors";
pub(crate) const METRIC_GC_BACKLOG_OLDEST_AGE_SECONDS: &str =
    "ironrag.maintenance.vector_lifecycle.gc.backlog_oldest_age_seconds";
pub(crate) const METRIC_GC_LAST_SUCCESS_UNIX_SECONDS: &str =
    "ironrag.maintenance.vector_lifecycle.gc.last_success_unix_seconds";

/// The bounded outcome label. Its values are exactly the contract's `outcome`
/// domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GcRunOutcome {
    Success,
    SkippedActiveIngest,
    SkippedLibraryBusy,
    Failed,
}

impl GcRunOutcome {
    pub(crate) const ALL: [Self; 4] =
        [Self::Success, Self::SkippedActiveIngest, Self::SkippedLibraryBusy, Self::Failed];

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::SkippedActiveIngest => "skipped_active_ingest",
            Self::SkippedLibraryBusy => "skipped_library_busy",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub(crate) const fn from_skip_reason(reason: GcSkipReason) -> Self {
        match reason {
            GcSkipReason::ActiveIngest => Self::SkippedActiveIngest,
            GcSkipReason::LibraryBusy => Self::SkippedLibraryBusy,
        }
    }
}

/// The bounded reason a soft skip occurred. A catalog-busy library is a skip
/// exactly like an active ingest, never a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GcSkipReason {
    ActiveIngest,
    LibraryBusy,
}

impl GcSkipReason {
    pub(crate) const ALL: [Self; 2] = [Self::ActiveIngest, Self::LibraryBusy];

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ActiveIngest => "active_ingest",
            Self::LibraryBusy => "library_busy",
        }
    }
}

/// Why a chunk vector was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeletionReason {
    NullHead,
    SupersededReadableHistory,
}

impl DeletionReason {
    pub(crate) const ALL: [Self; 2] = [Self::NullHead, Self::SupersededReadableHistory];

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NullHead => "null_head",
            Self::SupersededReadableHistory => "superseded_readable_history",
        }
    }
}

/// The bounded error-code set. It is exactly the contract's `error.code`
/// domain; granular internal errors collapse onto the declared values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GcErrorCode {
    CatalogLookup,
    LeaseComplete,
    LeaseCursor,
    LeaseFail,
    LeaseRequeue,
    Postgres,
    RuntimeGraph,
    UnsupportedClassScope,
    VectorPlane,
}

impl GcErrorCode {
    pub(crate) const ALL: [Self; 9] = [
        Self::CatalogLookup,
        Self::LeaseComplete,
        Self::LeaseCursor,
        Self::LeaseFail,
        Self::LeaseRequeue,
        Self::Postgres,
        Self::RuntimeGraph,
        Self::UnsupportedClassScope,
        Self::VectorPlane,
    ];

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CatalogLookup => "catalog_lookup",
            Self::LeaseComplete => "lease_complete",
            Self::LeaseCursor => "lease_cursor",
            Self::LeaseFail => "lease_fail",
            Self::LeaseRequeue => "lease_requeue",
            Self::Postgres => "postgres",
            Self::RuntimeGraph => "runtime_graph",
            Self::UnsupportedClassScope => "unsupported_class_scope",
            Self::VectorPlane => "vector_plane",
        }
    }
}

/// Classification of a dispatch error into the contract's bounded vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GcDispatchError {
    Skipped(GcSkipReason),
    Failed(GcErrorCode),
}

/// Map every GC error onto a soft skip or a bounded failure code.
///
/// `LibraryBusy` is a skip with reason `library_busy`; it must never collapse
/// onto `active_ingest` and must never be reported as a failure. Cursor and
/// budget variants collapse onto the declared `runtime_graph` code.
#[must_use]
pub(crate) const fn classify_gc_error(error: &GcError) -> GcDispatchError {
    match error {
        GcError::ActiveIngest { .. }
        | GcError::Graph(graph_gc::GraphGcError::ActiveIngest { .. }) => {
            GcDispatchError::Skipped(GcSkipReason::ActiveIngest)
        }
        GcError::LibraryBusy { .. }
        | GcError::Graph(graph_gc::GraphGcError::LibraryBusy { .. }) => {
            GcDispatchError::Skipped(GcSkipReason::LibraryBusy)
        }
        GcError::Sqlx(_) => GcDispatchError::Failed(GcErrorCode::Postgres),
        GcError::Vector(_) => GcDispatchError::Failed(GcErrorCode::VectorPlane),
        GcError::Graph(_) => GcDispatchError::Failed(GcErrorCode::RuntimeGraph),
        GcError::RuntimeGraphCursor(_)
        | GcError::UnsupportedRuntimeGraphCursorVersion { .. }
        | GcError::InvalidPhysicalMutationBudget(_) => {
            GcDispatchError::Failed(GcErrorCode::RuntimeGraph)
        }
    }
}

/// Vector kind label. `chunk` vectors carry a revision-lifecycle category;
/// `entity` vectors do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VectorKind {
    Chunk,
    Entity,
}

impl VectorKind {
    pub(crate) const ALL: [Self; 2] = [Self::Chunk, Self::Entity];

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Chunk => "chunk",
            Self::Entity => "entity",
        }
    }
}

/// Canonical chunk-vector lifecycle category. The four values mirror
/// `scripts/ops/collect_vector_lifecycle_state.py` and the contract's
/// `LIFECYCLE_CATEGORIES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleCategory {
    CurrentReadable,
    ProtectedActiveIngest,
    SupersededReadableHistory,
    InconsistentOrOrphan,
}

impl LifecycleCategory {
    pub(crate) const ALL: [Self; 4] = [
        Self::CurrentReadable,
        Self::ProtectedActiveIngest,
        Self::SupersededReadableHistory,
        Self::InconsistentOrOrphan,
    ];

    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CurrentReadable => "currentReadable",
            Self::ProtectedActiveIngest => "protectedActiveIngest",
            Self::SupersededReadableHistory => "supersededReadableHistory",
            Self::InconsistentOrOrphan => "inconsistentOrOrphan",
        }
    }

    /// The integer tag the bounded probe SQL classifies a chunk vector with.
    /// It mirrors `scripts/ops/collect_vector_lifecycle_state.py`'s
    /// `CATEGORY_NAMES` order and the tags in
    /// `infra::postgres::pg_search_store::vector_inventory`.
    #[must_use]
    pub(crate) const fn from_tag(tag: i64) -> Self {
        match tag {
            0 => Self::CurrentReadable,
            1 => Self::ProtectedActiveIngest,
            2 => Self::SupersededReadableHistory,
            _ => Self::InconsistentOrOrphan,
        }
    }
}
/// One completed `gc.superseded-vectors` or `gc.runtime-graph` run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GcRunObservation {
    pub(crate) maintenance_class: &'static str,
    pub(crate) outcome: GcRunOutcome,
    pub(crate) skip_reason: Option<GcSkipReason>,
    pub(crate) error_code: Option<GcErrorCode>,
    pub(crate) candidate_documents: u64,
    pub(crate) deleted_superseded_vectors: u64,
    pub(crate) deleted_null_head_vectors: u64,
    pub(crate) duration: Duration,
}

/// Per-category vector inventory for one library.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VectorInventoryObservation {
    pub(crate) vector_kind: VectorKind,
    pub(crate) dimensions: u32,
    pub(crate) lifecycle_category: LifecycleCategory,
    pub(crate) vector_rows: u64,
    pub(crate) logical_embedding_value_bytes: u64,
}

/// Physical relation size for one `(kind, dimensions)` shard, shared by every
/// library and deliberately without a library or lifecycle label.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PhysicalShardObservation {
    pub(crate) vector_kind: VectorKind,
    pub(crate) dimensions: u32,
    pub(crate) physical_shard_bytes: u64,
}

/// Bounded post-sweep backlog sample for the vector-lifecycle class.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GcBacklogObservation {
    pub(crate) vectors: u64,
    pub(crate) oldest_age_seconds: Option<f64>,
}

/// One global inventory sample.
#[derive(Debug, Clone)]
pub(crate) struct InventorySnapshot {
    pub(crate) sampled_at_unix_seconds: f64,
    pub(crate) vector_inventory: Vec<VectorInventoryObservation>,
    pub(crate) physical_shards: Vec<PhysicalShardObservation>,
    pub(crate) backlog: Option<GcBacklogObservation>,
    pub(crate) gc_last_success_unix_seconds: Option<f64>,
}

/// The 13 instruments declared by `VECTOR_LIFECYCLE_METRICS`, in order.
pub(crate) struct VectorLifecycleInstruments {
    inventory_vector_rows: Gauge<u64>,
    inventory_logical_embedding_value_bytes: Gauge<u64>,
    inventory_physical_shard_bytes: Gauge<u64>,
    inventory_last_success_unix_seconds: Gauge<f64>,
    gc_runs: Counter<u64>,
    gc_candidate_documents: Counter<u64>,
    gc_deleted_vectors: Counter<u64>,
    gc_skipped_runs: Counter<u64>,
    gc_errors: Counter<u64>,
    gc_duration_seconds: Histogram<f64>,
    gc_backlog_vectors: Gauge<u64>,
    gc_backlog_oldest_age_seconds: Gauge<f64>,
    gc_last_success_unix_seconds: Gauge<f64>,
}

impl VectorLifecycleInstruments {
    /// Build every instrument from the supplied meter. Reading the contract
    /// here fails fast in debug builds if its names, units or label domains
    /// ever stop matching this module.
    fn new(meter: &Meter) -> Self {
        debug_assert!(
            contract_is_well_formed(),
            "vector-lifecycle metric contract and label domains must stay consistent",
        );
        Self {
            inventory_vector_rows: meter
                .u64_gauge(METRIC_INVENTORY_VECTOR_ROWS)
                .with_description(
                    "Chunk vectors per library, kind, dimensions and lifecycle category",
                )
                .with_unit("{vector}")
                .build(),
            inventory_logical_embedding_value_bytes: meter
                .u64_gauge(METRIC_INVENTORY_LOGICAL_EMBEDDING_VALUE_BYTES)
                .with_description("Sum of logical embedding datum sizes for one library and shard")
                .with_unit("By")
                .build(),
            inventory_physical_shard_bytes: meter
                .u64_gauge(METRIC_INVENTORY_PHYSICAL_SHARD_BYTES)
                .with_description("Exact shared relation size for one vector shard")
                .with_unit("By")
                .build(),
            inventory_last_success_unix_seconds: meter
                .f64_gauge(METRIC_INVENTORY_LAST_SUCCESS_UNIX_SECONDS)
                .with_description("Unix timestamp of the last successful inventory sample")
                .with_unit("s")
                .build(),
            gc_runs: meter
                .u64_counter(METRIC_GC_RUNS)
                .with_description("Completed vector-lifecycle GC runs by outcome")
                .with_unit("{run}")
                .build(),
            gc_candidate_documents: meter
                .u64_counter(METRIC_GC_CANDIDATE_DOCUMENTS)
                .with_description(
                    "Documents with superseded vectors examined by a successful sweep",
                )
                .with_unit("{document}")
                .build(),
            gc_deleted_vectors: meter
                .u64_counter(METRIC_GC_DELETED_VECTORS)
                .with_description("Chunk vectors deleted by reason")
                .with_unit("{vector}")
                .build(),
            gc_skipped_runs: meter
                .u64_counter(METRIC_GC_SKIPPED_RUNS)
                .with_description("GC runs deferred by a soft skip reason")
                .with_unit("{run}")
                .build(),
            gc_errors: meter
                .u64_counter(METRIC_GC_ERRORS)
                .with_description("Failed vector-lifecycle GC runs by bounded error code")
                .with_unit("{error}")
                .build(),
            gc_duration_seconds: meter
                .f64_histogram(METRIC_GC_DURATION_SECONDS)
                .with_description("Wall time of one GC dispatch")
                .with_unit("s")
                .build(),
            gc_backlog_vectors: meter
                .u64_gauge(METRIC_GC_BACKLOG_VECTORS)
                .with_description("Superseded vectors remaining in the bounded backlog sample")
                .with_unit("{vector}")
                .build(),
            gc_backlog_oldest_age_seconds: meter
                .f64_gauge(METRIC_GC_BACKLOG_OLDEST_AGE_SECONDS)
                .with_description("Age of the oldest vector in the bounded backlog sample")
                .with_unit("s")
                .build(),
            gc_last_success_unix_seconds: meter
                .f64_gauge(METRIC_GC_LAST_SUCCESS_UNIX_SECONDS)
                .with_description("Unix timestamp of the last successful vector-lifecycle GC run")
                .with_unit("s")
                .build(),
        }
    }

    /// Process-wide instruments on the shared maintenance meter.
    #[must_use]
    pub(crate) fn global() -> &'static Self {
        static INSTRUMENTS: OnceLock<VectorLifecycleInstruments> = OnceLock::new();
        INSTRUMENTS.get_or_init(|| Self::new(&global::meter(METER_NAME)))
    }

    /// Record one completed GC run. The success gauge, the deleted-vector
    /// counters and the candidate-document counter are only touched by a real
    /// success for a vector-lifecycle-scoped class; skipped and failed runs
    /// never advance them. A dispatch failure is always recorded, whether it
    /// came from a vector-lifecycle class or not, so a failure is never hidden
    /// behind a healthy sibling's success.
    pub(crate) fn record_gc_run(&self, observation: &GcRunObservation) {
        let class = KeyValue::new(LABEL_MAINTENANCE_CLASS, observation.maintenance_class);
        let outcome = KeyValue::new(LABEL_OUTCOME, observation.outcome.label());
        self.gc_runs.add(1, &[class.clone(), outcome.clone()]);
        self.gc_duration_seconds
            .record(observation.duration.as_secs_f64(), &[class.clone(), outcome.clone()]);

        let vector_scope =
            observation_scope(observation.maintenance_class) == ObservationScope::VectorLifecycle;
        if vector_scope && observation.outcome == GcRunOutcome::Success {
            self.gc_candidate_documents
                .add(observation.candidate_documents, std::slice::from_ref(&class));
            self.gc_deleted_vectors.add(
                observation.deleted_superseded_vectors,
                &[
                    class.clone(),
                    KeyValue::new(
                        LABEL_DELETION_REASON,
                        DeletionReason::SupersededReadableHistory.label(),
                    ),
                ],
            );
            self.gc_deleted_vectors.add(
                observation.deleted_null_head_vectors,
                &[
                    class.clone(),
                    KeyValue::new(LABEL_DELETION_REASON, DeletionReason::NullHead.label()),
                ],
            );
        }

        if let Some(reason) = observation.skip_reason {
            self.gc_skipped_runs
                .add(1, &[class.clone(), KeyValue::new(LABEL_SKIP_REASON, reason.label())]);
        }
        if let Some(error_code) = observation.error_code {
            self.gc_errors
                .add(1, &[class.clone(), KeyValue::new(LABEL_ERROR_CODE, error_code.label())]);
        }
        if observation.outcome == GcRunOutcome::Success {
            self.gc_last_success_unix_seconds.record(unix_now_seconds(), &[class]);
        }
    }

    /// Record one global inventory sample: vector counts per shared shard and
    /// lifecycle category, shared physical shard sizes, and the bounded backlog
    /// plus last-success timestamps. No library identity is emitted; a
    /// per-library drill-down is operator SQL and structured logs.
    pub(crate) fn record_inventory_snapshot(&self, snapshot: &InventorySnapshot) {
        for row in &snapshot.vector_inventory {
            let attributes = [
                KeyValue::new(LABEL_VECTOR_KIND, row.vector_kind.label()),
                KeyValue::new(LABEL_VECTOR_DIMENSIONS, i64::from(row.dimensions)),
                KeyValue::new(LABEL_LIFECYCLE_CATEGORY, row.lifecycle_category.label()),
            ];
            self.inventory_vector_rows.record(row.vector_rows, &attributes);
            self.inventory_logical_embedding_value_bytes
                .record(row.logical_embedding_value_bytes, &attributes);
        }
        for shard in &snapshot.physical_shards {
            let attributes = [
                KeyValue::new(LABEL_VECTOR_KIND, shard.vector_kind.label()),
                KeyValue::new(LABEL_VECTOR_DIMENSIONS, i64::from(shard.dimensions)),
            ];
            self.inventory_physical_shard_bytes.record(shard.physical_shard_bytes, &attributes);
        }
        self.inventory_last_success_unix_seconds.record(snapshot.sampled_at_unix_seconds, &[]);

        if let Some(backlog) = snapshot.backlog {
            let class = KeyValue::new(LABEL_MAINTENANCE_CLASS, vector_lifecycle_class());
            self.gc_backlog_vectors.record(backlog.vectors, std::slice::from_ref(&class));
            self.gc_backlog_oldest_age_seconds
                .record(backlog.oldest_age_seconds.unwrap_or(0.0), std::slice::from_ref(&class));
        }
        if let Some(timestamp) = snapshot.gc_last_success_unix_seconds {
            let class = KeyValue::new(LABEL_MAINTENANCE_CLASS, vector_lifecycle_class());
            self.gc_last_success_unix_seconds.record(timestamp, std::slice::from_ref(&class));
        }
    }
}

/// The maintenance class the contract scopes to vector lifecycle.
#[must_use]
pub(crate) fn vector_lifecycle_class() -> &'static str {
    MaintenanceClass::GcSupersededVectors.as_str()
}

fn observation_scope(maintenance_class: &str) -> ObservationScope {
    MAINTENANCE_OBSERVATION_CONTRACTS
        .iter()
        .find(|contract| contract.maintenance_class == maintenance_class)
        .map_or(ObservationScope::Other, |contract| contract.scope)
}

fn unix_now_seconds() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Validate that every label domain, metric definition and enum label agrees
/// with the contract. This runs only in debug builds; the reference keeps the
/// contract fields live.
fn contract_is_well_formed() -> bool {
    let mut metric_names = BTreeSet::new();
    let metrics_ok = VECTOR_LIFECYCLE_METRICS.iter().all(|definition: &MetricDefinition| {
        matches!(
            definition.instrument,
            InstrumentKind::Counter | InstrumentKind::Gauge | InstrumentKind::Histogram
        ) && metric_names.insert(definition.name)
            && !definition.unit.is_empty()
            // A label-free gauge is a valid declaration: `inventory.last_success`
            // is deliberately global and carries no identity attribute.
            && definition.attributes.iter().all(|attribute| !attribute.is_empty())
    });
    let mut domain_labels = BTreeSet::new();
    let domains_ok = BOUNDED_LABEL_VALUE_DOMAINS
        .iter()
        .all(|domain| domain_labels.insert(domain.label) && !domain.values.is_empty());
    metrics_ok
        && domains_ok
        // No vector-lifecycle metric may declare the library identity label: it
        // grows with the catalog and overflows the SDK series ceiling. Identity
        // stays in operator SQL and structured logs.
        && VECTOR_LIFECYCLE_METRICS
            .iter()
            .all(|definition| !definition.attributes.contains(&LABEL_LIBRARY_ID))
        && LIFECYCLE_CATEGORIES == LifecycleCategory::ALL.map(LifecycleCategory::label).as_slice()
        && labels_are_bounded(LABEL_OUTCOME, GcRunOutcome::ALL.iter().map(|value| value.label()))
        && labels_are_bounded(
            LABEL_SKIP_REASON,
            GcSkipReason::ALL.iter().map(|value| value.label()),
        )
        && labels_are_bounded(
            LABEL_DELETION_REASON,
            DeletionReason::ALL.iter().map(|value| value.label()),
        )
        && labels_are_bounded(LABEL_ERROR_CODE, GcErrorCode::ALL.iter().map(|value| value.label()))
        && labels_are_bounded(LABEL_VECTOR_KIND, VectorKind::ALL.iter().map(|value| value.label()))
}

fn labels_are_bounded(label: &str, values: impl Iterator<Item = &'static str>) -> bool {
    BOUNDED_LABEL_VALUE_DOMAINS
        .iter()
        .find(|domain| domain.label == label)
        .is_some_and(|domain| values.into_iter().all(|value| domain.values.contains(&value)))
}

#[cfg(test)]
mod tests;
