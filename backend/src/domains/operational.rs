use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unavailable,
    Misconfigured,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyStatusSummary {
    pub name: String,
    pub state: HealthState,
    pub summary: String,
    pub remediation_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalStatusSummary {
    pub state: HealthState,
    pub summary: String,
    pub dependencies: Vec<DependencyStatusSummary>,
}
