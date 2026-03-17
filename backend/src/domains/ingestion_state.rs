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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiIngestionStatus {
    pub status: String,
    pub stage: String,
    pub progress_percent: Option<u8>,
}

#[must_use]
pub fn map_to_ui_status(status: &str, stage: &str) -> UiIngestionStatus {
    let normalized_stage = match stage.trim() {
        "" => "created",
        value => value,
    };

    match status {
        "ready_no_graph" => UiIngestionStatus {
            status: "ready_no_graph".to_string(),
            stage: normalized_stage.to_string(),
            progress_percent: Some(100),
        },
        "completed" => UiIngestionStatus {
            status: "ready".to_string(),
            stage: "completed".to_string(),
            progress_percent: Some(100),
        },
        "failed" | "retryable_failed" | "partial" | "canceled" => UiIngestionStatus {
            status: "failed".to_string(),
            stage: normalized_stage.to_string(),
            progress_percent: None,
        },
        "running" | "validating" => UiIngestionStatus {
            status: "processing".to_string(),
            stage: normalized_stage.to_string(),
            progress_percent: progress_for_stage(normalized_stage),
        },
        "queued" => UiIngestionStatus {
            status: "queued".to_string(),
            stage: queue_stage_label(normalized_stage).to_string(),
            progress_percent: None,
        },
        _ => UiIngestionStatus {
            status: "queued".to_string(),
            stage: normalized_stage.to_string(),
            progress_percent: None,
        },
    }
}

fn queue_stage_label(stage: &str) -> &str {
    match stage {
        "created" => "upload_received",
        other => other,
    }
}

fn progress_for_stage(stage: &str) -> Option<u8> {
    match stage {
        "created" | "claimed" | "reclaimed_after_lease_expiry" | "upload_received" => Some(10),
        "accepted" => Some(5),
        "extracting_content" | "extracting_text" => Some(30),
        "embedding_chunks" => Some(75),
        "persisting_document" => Some(55),
        "chunking" => Some(80),
        "building_graph" => Some(92),
        "finalizing" => Some(95),
        "completed" => Some(100),
        _ => Some(45),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_completed_job_to_ready_ui_state() {
        let state = map_to_ui_status("completed", "completed");
        assert_eq!(state.status, "ready");
        assert_eq!(state.progress_percent, Some(100));
    }

    #[test]
    fn maps_queued_created_job_to_upload_received() {
        let state = map_to_ui_status("queued", "created");
        assert_eq!(state.status, "queued");
        assert_eq!(state.stage, "upload_received");
    }

    #[test]
    fn maps_retryable_failed_job_to_failed_ui_state() {
        let state = map_to_ui_status("retryable_failed", "failed");
        assert_eq!(state.status, "failed");
        assert_eq!(state.progress_percent, None);
    }

    #[test]
    fn maps_ready_no_graph_to_terminal_ui_state() {
        let state = map_to_ui_status("ready_no_graph", "finalizing");
        assert_eq!(state.status, "ready_no_graph");
        assert_eq!(state.progress_percent, Some(100));
    }
}
