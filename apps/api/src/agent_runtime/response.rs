use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRecoveryOutcome {
    pub attempts: u8,
    pub summary_redacted: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFailureSummary {
    pub code: String,
    pub summary_redacted: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTerminalOutcome<TSuccess, TFailure> {
    Completed { success: TSuccess },
    Recovered { success: TSuccess, recovery: RuntimeRecoveryOutcome },
    Failed { failure: TFailure, summary: RuntimeFailureSummary },
    Canceled { failure: TFailure, summary: RuntimeFailureSummary },
}
