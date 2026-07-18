//! Health, warning, and diagnostic state contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Severity attached to an operator-visible diagnostic.
pub enum MessageLevel {
    /// Informational state that requires no intervention.
    Info,
    /// Degraded or unusual state that merits attention.
    Warning,
    /// Failure that prevents the represented operation or surface from working.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Stable, human-readable warning exposed on an operator surface.
pub struct OperatorWarning {
    /// Machine-stable identifier suitable for filtering and support links.
    pub code: String,
    /// Operational severity of the condition.
    pub level: MessageLevel,
    /// Short summary intended for compact UI presentation.
    pub title: String,
    /// Actionable explanation of the condition and its impact.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Lifecycle of data loaded asynchronously for a client surface.
pub enum LoadStatus {
    /// Loading has not started.
    Idle,
    /// A load operation is in progress.
    Loading,
    /// A usable value is available.
    Ready,
    /// Loading succeeded but produced no value.
    Empty,
    /// Loading failed; `message` should explain the failure.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Data and status for an asynchronously populated client surface.
pub struct LoadState<T> {
    /// Current phase of the load lifecycle.
    pub status: LoadStatus,
    /// Loaded payload, present only when the state carries usable data.
    pub value: Option<T>,
    /// Explanation for empty, delayed, or failed loads when available.
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// One independently identifiable reason a surface is degraded.
pub struct DegradedState {
    /// Machine-stable identifier for the degradation reason.
    pub code: String,
    /// Concise operator-facing description.
    pub summary: String,
    /// Optional diagnostic context or remediation guidance.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Aggregate usability of an operator-facing surface.
pub enum SurfaceHealth {
    /// All required capabilities are working.
    Healthy,
    /// The surface remains usable with reduced capability or stale data.
    Degraded,
    /// A required dependency prevents meaningful use.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Named metric included in an operator diagnostic summary.
pub struct DiagnosticCounter {
    /// Machine-stable metric identifier.
    pub key: String,
    /// Human-readable metric label.
    pub label: String,
    /// Current integer measurement.
    pub value: i32,
    /// Severity implied by the current measurement.
    pub level: MessageLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Aggregate health, metrics, and explanations for one UI surface.
pub struct SurfaceDiagnostics {
    /// Overall usability derived from the accompanying diagnostics.
    pub health: SurfaceHealth,
    /// Measurements that summarize the surface state.
    pub counters: Vec<DiagnosticCounter>,
    /// Conditions that operators may need to investigate.
    pub warnings: Vec<OperatorWarning>,
    /// Specific causes of reduced functionality.
    pub degraded: Vec<DegradedState>,
    /// Time at which this diagnostic snapshot was assembled.
    pub updated_at: Option<DateTime<Utc>>,
}
