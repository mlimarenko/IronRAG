use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum IngestionLifecycleState {
    Queued,
    Validating,
    Running,
    Partial,
    Completed,
    Failed,
    RetryableFailed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionJobStatusSummary {
    pub job_id: Uuid,
    pub state: IngestionLifecycleState,
    pub stage: String,
    pub retryable: bool,
    pub summary: String,
}
