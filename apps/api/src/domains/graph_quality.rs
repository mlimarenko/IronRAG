use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionOutcomeStatus {
    Clean,
    Recovered,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionRecoverySummary {
    pub status: ExtractionOutcomeStatus,
    pub second_pass_applied: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphSummaryConfidenceStatus {
    Strong,
    Partial,
    Weak,
    Conflicted,
}
