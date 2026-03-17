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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalGraphSummary {
    pub text: String,
    pub confidence_status: GraphSummaryConfidenceStatus,
    pub support_count: usize,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationImpactScopeStatus {
    Pending,
    Targeted,
    FallbackBroad,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationImpactScopeConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationImpactScopeSummary {
    pub scope_status: MutationImpactScopeStatus,
    pub confidence_status: MutationImpactScopeConfidence,
    pub affected_node_count: usize,
    pub affected_relationship_count: usize,
    pub fallback_reason: Option<String>,
}
