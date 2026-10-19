use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::agent_runtime::RuntimeTaskKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PricingBillingUnit {
    Per1MInputTokens,
    Per1MCachedInputTokens,
    #[serde(rename = "per_1m_cache_write_input_tokens")]
    Per1MCacheWriteInputTokens,
    Per1MOutputTokens,
    Per1MTokens,
    FixedPerCall,
}

impl PricingBillingUnit {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Per1MInputTokens => "per_1m_input_tokens",
            Self::Per1MCachedInputTokens => "per_1m_cached_input_tokens",
            Self::Per1MCacheWriteInputTokens => "per_1m_cache_write_input_tokens",
            Self::Per1MOutputTokens => "per_1m_output_tokens",
            Self::Per1MTokens => "per_1m_tokens",
            Self::FixedPerCall => "fixed_per_call",
        }
    }
}

/// Whether a reported cost covers every provider call in its scope.
///
/// A price catalog entry is operator-configured, so a call can produce usage
/// with no charge. Without this status a caller cannot tell an execution that
/// genuinely cost nothing from one whose price was never configured — both
/// report a zero total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CostCoverageStatus {
    /// Every provider call in scope is priced. The total is the full cost.
    Complete,
    /// Some calls are priced and some are not. The total is a lower bound.
    Partial,
    /// No call in scope is priced while unpriced calls exist. The total
    /// carries no information about what the scope cost.
    Unknown,
}

impl CostCoverageStatus {
    /// Derives coverage from the priced/unpriced split of one scope.
    #[must_use]
    pub const fn from_call_counts(
        provider_call_count: i64,
        unpriced_provider_call_count: i64,
    ) -> Self {
        if unpriced_provider_call_count <= 0 {
            Self::Complete
        } else if unpriced_provider_call_count >= provider_call_count {
            Self::Unknown
        } else {
            Self::Partial
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether a cost read reflects every committed provider call in its scope.
///
/// Independent of [`CostCoverageStatus`], which is a *pricing* fact: a scope
/// can be fully priced yet still be reconciling a provider call that has not
/// been folded into its rollup, and the reverse. A `Reconciling` read withholds
/// its cost fields instead of serving a stale aggregate as exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CostRollupFreshness {
    /// Every committed provider call in scope is folded into the rollup.
    Current,
    /// At least one owning execution's rollup lags a committed provider call,
    /// so the reported cost aggregate would be stale if served.
    Reconciling,
}

impl CostRollupFreshness {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Reconciling => "reconciling",
        }
    }

    /// Derives freshness from the owning scope's rollup-health probe.
    #[must_use]
    pub const fn from_rollup_dirty(rollup_dirty: bool) -> Self {
        if rollup_dirty { Self::Reconciling } else { Self::Current }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BillingExecutionOwnerKind {
    QueryExecution,
    GraphExtractionAttempt,
    IngestAttempt,
}

impl BillingExecutionOwnerKind {
    /// Exact `billing_owning_execution_kind` Postgres enum key. Also
    /// used as the wire string for `executionKind` HTTP path segments (kept
    /// in sync with the `#[serde(rename_all = "snake_case")]` above, which
    /// axum's `Path` extractor uses directly to parse this type from a
    /// single path segment).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryExecution => "query_execution",
            Self::GraphExtractionAttempt => "graph_extraction_attempt",
            Self::IngestAttempt => "ingest_attempt",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingProviderCall {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub owning_execution_kind: BillingExecutionOwnerKind,
    pub owning_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<RuntimeTaskKind>,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub call_kind: String,
    pub call_state: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingCharge {
    pub id: Uuid,
    pub usage_id: Uuid,
    pub currency_code: String,
    pub unit_price: Decimal,
    pub total_price: Decimal,
    pub priced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingExecutionCost {
    pub id: Uuid,
    pub owning_execution_kind: BillingExecutionOwnerKind,
    pub owning_execution_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i32,
    pub unpriced_provider_call_count: i32,
    pub cost_coverage: CostCoverageStatus,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod cost_coverage_tests {
    use super::CostCoverageStatus;

    #[test]
    fn coverage_separates_free_scopes_from_unpriced_ones() {
        // No calls at all: the zero total is the whole truth.
        assert_eq!(CostCoverageStatus::from_call_counts(0, 0), CostCoverageStatus::Complete);
        // Every call priced.
        assert_eq!(CostCoverageStatus::from_call_counts(4, 0), CostCoverageStatus::Complete);
        // Some priced, some not: the total is a lower bound.
        assert_eq!(CostCoverageStatus::from_call_counts(4, 1), CostCoverageStatus::Partial);
        // Nothing priced: a zero total says nothing about what was spent.
        assert_eq!(CostCoverageStatus::from_call_counts(4, 4), CostCoverageStatus::Unknown);
    }

    #[test]
    fn coverage_is_unknown_when_unpriced_calls_exceed_the_counted_calls() {
        // Defensive: a rollup recomputed between the two counts must not
        // report better coverage than it can prove.
        assert_eq!(CostCoverageStatus::from_call_counts(2, 5), CostCoverageStatus::Unknown);
    }
}

#[cfg(test)]
mod cost_rollup_freshness_tests {
    use super::CostRollupFreshness;

    #[test]
    fn freshness_is_independent_of_pricing_coverage() {
        // A clean generation is `current` no matter what the coverage says; a
        // dirty one is `reconciling` and withholds its cost fields.
        assert_eq!(CostRollupFreshness::from_rollup_dirty(false), CostRollupFreshness::Current);
        assert_eq!(CostRollupFreshness::from_rollup_dirty(true), CostRollupFreshness::Reconciling);
        assert_eq!(CostRollupFreshness::Current.as_str(), "current");
        assert_eq!(CostRollupFreshness::Reconciling.as_str(), "reconciling");
    }
}
