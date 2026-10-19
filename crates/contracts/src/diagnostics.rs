//! Health, warning, and diagnostic state contracts.

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
