use chrono::{DateTime, Duration, Utc};

use crate::domains::runtime_ingestion::{RuntimeDocumentActivityStatus, RuntimeIngestionStatus};

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
}
