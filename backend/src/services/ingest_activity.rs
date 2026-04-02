use chrono::{DateTime, Duration, Utc};

use crate::domains::{
    content::{ContentDocumentPipelineJob, ContentMutation},
    runtime_ingestion::{RuntimeDocumentActivityStatus, RuntimeIngestionStatus},
};

#[derive(Debug, Clone)]
pub struct IngestActivityService {
    freshness_window: Duration,
    stalled_after: Duration,
}

impl Default for IngestActivityService {
    fn default() -> Self {
        Self::new(45, 180)
    }
}

impl IngestActivityService {
    #[must_use]
    pub fn new(freshness_seconds: u64, stalled_after_seconds: u64) -> Self {
        Self {
            freshness_window: Duration::seconds(i64::try_from(freshness_seconds).unwrap_or(45)),
            stalled_after: Duration::seconds(i64::try_from(stalled_after_seconds).unwrap_or(180)),
        }
    }

    #[must_use]
    pub fn is_activity_fresh(
        &self,
        last_activity_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        last_activity_at.is_some_and(|value| now - value <= self.freshness_window)
    }

    #[must_use]
    pub fn derive_status(
        &self,
        run_status: RuntimeIngestionStatus,
        claimed_at: Option<DateTime<Utc>>,
        last_activity_at: Option<DateTime<Utc>>,
        latest_error: Option<&str>,
        now: DateTime<Utc>,
    ) -> RuntimeDocumentActivityStatus {
        match run_status {
            RuntimeIngestionStatus::Ready | RuntimeIngestionStatus::ReadyNoGraph => {
                RuntimeDocumentActivityStatus::Ready
            }
            RuntimeIngestionStatus::Failed => RuntimeDocumentActivityStatus::Failed,
            RuntimeIngestionStatus::Queued => {
                derive_queued_status(claimed_at, latest_error, now, self.stalled_after)
            }
            RuntimeIngestionStatus::Processing => {
                if self.is_activity_fresh(last_activity_at, now) {
                    RuntimeDocumentActivityStatus::Active
                } else if latest_error.is_some_and(is_blocked_message) {
                    RuntimeDocumentActivityStatus::Blocked
                } else if latest_error.is_some_and(is_retry_message) {
                    RuntimeDocumentActivityStatus::Retrying
                } else {
                    RuntimeDocumentActivityStatus::Stalled
                }
            }
        }
    }

    #[must_use]
    pub fn stalled_reason(
        &self,
        run_status: RuntimeIngestionStatus,
        claimed_at: Option<DateTime<Utc>>,
        last_activity_at: Option<DateTime<Utc>>,
        latest_error: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<String> {
        match run_status {
            RuntimeIngestionStatus::Queued => {
                let status =
                    derive_queued_status(claimed_at, latest_error, now, self.stalled_after);
                if status != RuntimeDocumentActivityStatus::Stalled {
                    return None;
                }
            }
            RuntimeIngestionStatus::Processing => {
                if self.is_activity_fresh(last_activity_at, now) {
                    return None;
                }
            }
            RuntimeIngestionStatus::Ready
            | RuntimeIngestionStatus::ReadyNoGraph
            | RuntimeIngestionStatus::Failed => return None,
        }
        if let Some(message) = latest_error.filter(|message| !message.trim().is_empty()) {
            return Some(message.trim().to_string());
        }
        let idle_since = match run_status {
            RuntimeIngestionStatus::Queued => claimed_at,
            RuntimeIngestionStatus::Processing => last_activity_at,
            RuntimeIngestionStatus::Ready
            | RuntimeIngestionStatus::ReadyNoGraph
            | RuntimeIngestionStatus::Failed => None,
        };
        idle_since.map(|value| {
            let idle = now - value;
            if idle >= self.stalled_after {
                match run_status {
                    RuntimeIngestionStatus::Queued => {
                        format!(
                            "claimed but no visible activity followed for {}s",
                            idle.num_seconds()
                        )
                    }
                    RuntimeIngestionStatus::Processing => {
                        format!("no visible activity for {}s", idle.num_seconds())
                    }
                    RuntimeIngestionStatus::Ready
                    | RuntimeIngestionStatus::ReadyNoGraph
                    | RuntimeIngestionStatus::Failed => {
                        "activity freshness window elapsed".to_string()
                    }
                }
            } else {
                "activity freshness window elapsed".to_string()
            }
        })
    }

    #[must_use]
    pub fn derive_document_activity(
        &self,
        latest_mutation: Option<&ContentMutation>,
        latest_job: Option<&ContentDocumentPipelineJob>,
        text_ready: bool,
        graph_ready: bool,
        now: DateTime<Utc>,
    ) -> RuntimeDocumentActivityStatus {
        let signal = document_activity_signal(latest_mutation, latest_job, text_ready, graph_ready);
        self.derive_status(
            signal.run_status,
            signal.claimed_at,
            signal.last_activity_at,
            signal.latest_error,
            now,
        )
    }

    #[must_use]
    pub fn document_stalled_reason(
        &self,
        latest_mutation: Option<&ContentMutation>,
        latest_job: Option<&ContentDocumentPipelineJob>,
        text_ready: bool,
        graph_ready: bool,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let signal = document_activity_signal(latest_mutation, latest_job, text_ready, graph_ready);
        self.stalled_reason(
            signal.run_status,
            signal.claimed_at,
            signal.last_activity_at,
            signal.latest_error,
            now,
        )
    }
}

struct DocumentActivitySignal<'a> {
    run_status: RuntimeIngestionStatus,
    claimed_at: Option<DateTime<Utc>>,
    last_activity_at: Option<DateTime<Utc>>,
    latest_error: Option<&'a str>,
}

fn document_activity_signal<'a>(
    latest_mutation: Option<&'a ContentMutation>,
    latest_job: Option<&'a ContentDocumentPipelineJob>,
    text_ready: bool,
    graph_ready: bool,
) -> DocumentActivitySignal<'a> {
    if let Some(job) = latest_job {
        let run_status = map_job_queue_state(&job.queue_state, text_ready, graph_ready);
        return DocumentActivitySignal {
            run_status,
            claimed_at: job.claimed_at,
            last_activity_at: job.last_activity_at.or(job.completed_at).or(Some(job.queued_at)),
            latest_error: job.failure_code.as_deref(),
        };
    }

    if let Some(mutation) = latest_mutation {
        let run_status = match mutation.mutation_state.as_str() {
            "accepted" | "running" => RuntimeIngestionStatus::Processing,
            "failed" | "conflicted" | "canceled" => RuntimeIngestionStatus::Failed,
            _ if graph_ready => RuntimeIngestionStatus::Ready,
            _ if text_ready => RuntimeIngestionStatus::ReadyNoGraph,
            _ => RuntimeIngestionStatus::Queued,
        };
        return DocumentActivitySignal {
            run_status,
            claimed_at: Some(mutation.requested_at),
            last_activity_at: mutation.completed_at.or(Some(mutation.requested_at)),
            latest_error: mutation.failure_code.as_deref(),
        };
    }

    DocumentActivitySignal {
        run_status: if graph_ready {
            RuntimeIngestionStatus::Ready
        } else if text_ready {
            RuntimeIngestionStatus::ReadyNoGraph
        } else {
            RuntimeIngestionStatus::Queued
        },
        claimed_at: None,
        last_activity_at: None,
        latest_error: None,
    }
}

fn map_job_queue_state(
    queue_state: &str,
    text_ready: bool,
    graph_ready: bool,
) -> RuntimeIngestionStatus {
    match queue_state {
        "queued" => RuntimeIngestionStatus::Queued,
        "leased" => RuntimeIngestionStatus::Processing,
        "completed" if graph_ready => RuntimeIngestionStatus::Ready,
        "completed" if text_ready => RuntimeIngestionStatus::ReadyNoGraph,
        "completed" => RuntimeIngestionStatus::Processing,
        "failed" | "canceled" => RuntimeIngestionStatus::Failed,
        _ => RuntimeIngestionStatus::Processing,
    }
}

fn derive_queued_status(
    claimed_at: Option<DateTime<Utc>>,
    latest_error: Option<&str>,
    now: DateTime<Utc>,
    stalled_after: Duration,
) -> RuntimeDocumentActivityStatus {
    if latest_error.is_some_and(is_retry_message) {
        RuntimeDocumentActivityStatus::Retrying
    } else if latest_error.is_some_and(is_blocked_message) {
        RuntimeDocumentActivityStatus::Blocked
    } else if claimed_at.is_some_and(|value| now - value >= stalled_after) {
        RuntimeDocumentActivityStatus::Stalled
    } else {
        RuntimeDocumentActivityStatus::Queued
    }
}

fn is_retry_message(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("retry") || lowered.contains("requeue")
}

fn is_blocked_message(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("blocked") || lowered.contains("waiting")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::content::{ContentDocumentPipelineJob, ContentMutation};
    use uuid::Uuid;

    #[test]
    fn processing_with_fresh_activity_is_active() {
        let service = IngestActivityService::new(45, 180);
        let now = Utc::now();

        assert_eq!(
            service.derive_status(
                RuntimeIngestionStatus::Processing,
                None,
                Some(now - Duration::seconds(10)),
                None,
                now,
            ),
            RuntimeDocumentActivityStatus::Active
        );
    }

    #[test]
    fn queued_retry_message_maps_to_retrying() {
        let service = IngestActivityService::default();
        let now = Utc::now();

        assert_eq!(
            service.derive_status(
                RuntimeIngestionStatus::Queued,
                Some(now - Duration::seconds(15)),
                None,
                Some("worker heartbeat stalled before completion; requeued for retry"),
                now,
            ),
            RuntimeDocumentActivityStatus::Retrying
        );
    }

    #[test]
    fn queued_without_claim_stays_queued_even_when_old() {
        let service = IngestActivityService::new(45, 180);
        let now = Utc::now();

        assert_eq!(
            service.derive_status(
                RuntimeIngestionStatus::Queued,
                None,
                Some(now - Duration::seconds(300)),
                None,
                now,
            ),
            RuntimeDocumentActivityStatus::Queued
        );
        assert_eq!(
            service.stalled_reason(
                RuntimeIngestionStatus::Queued,
                None,
                Some(now - Duration::seconds(300)),
                None,
                now,
            ),
            None
        );
    }

    #[test]
    fn queued_with_stale_claim_becomes_stalled() {
        let service = IngestActivityService::new(45, 180);
        let now = Utc::now();

        assert_eq!(
            service.derive_status(
                RuntimeIngestionStatus::Queued,
                Some(now - Duration::seconds(300)),
                None,
                None,
                now,
            ),
            RuntimeDocumentActivityStatus::Stalled
        );
        assert_eq!(
            service.stalled_reason(
                RuntimeIngestionStatus::Queued,
                Some(now - Duration::seconds(300)),
                None,
                None,
                now,
            ),
            Some("claimed but no visible activity followed for 300s".to_string())
        );
    }

    #[test]
    fn leased_document_job_with_stale_heartbeat_becomes_stalled() {
        let service = IngestActivityService::new(45, 180);
        let now = Utc::now();
        let job = ContentDocumentPipelineJob {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            mutation_id: Some(Uuid::now_v7()),
            async_operation_id: None,
            job_kind: "content_mutation".to_string(),
            queue_state: "leased".to_string(),
            queued_at: now - Duration::seconds(600),
            available_at: now - Duration::seconds(600),
            completed_at: None,
            claimed_at: Some(now - Duration::seconds(300)),
            last_activity_at: Some(now - Duration::seconds(300)),
            current_stage: Some("extract_content".to_string()),
            failure_code: None,
            retryable: false,
        };

        assert_eq!(
            service.derive_document_activity(None, Some(&job), false, false, now),
            RuntimeDocumentActivityStatus::Stalled
        );
        assert_eq!(
            service.document_stalled_reason(None, Some(&job), false, false, now),
            Some("no visible activity for 300s".to_string())
        );
    }

    #[test]
    fn applied_document_mutation_without_job_is_ready_when_text_is_readable() {
        let service = IngestActivityService::default();
        let now = Utc::now();
        let mutation = ContentMutation {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            operation_kind: "web_capture".to_string(),
            mutation_state: "applied".to_string(),
            requested_at: now - Duration::seconds(10),
            completed_at: Some(now - Duration::seconds(5)),
            requested_by_principal_id: None,
            request_surface: "test".to_string(),
            idempotency_key: None,
            source_identity: None,
            failure_code: None,
            conflict_code: None,
        };

        assert_eq!(
            service.derive_document_activity(Some(&mutation), None, true, false, now),
            RuntimeDocumentActivityStatus::Ready
        );
        assert_eq!(service.document_stalled_reason(Some(&mutation), None, true, false, now), None);
    }
}
