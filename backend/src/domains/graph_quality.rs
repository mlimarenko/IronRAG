use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionOutcomeStatus {
    Clean,
    Recovered,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionRecoverySummary {
    pub status: ExtractionOutcomeStatus,
    pub parser_repair_applied: bool,
    pub second_pass_applied: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphSummaryConfidenceStatus {
    Strong,
    Partial,
    Weak,
    Conflicted,
}
