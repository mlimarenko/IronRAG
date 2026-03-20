use chrono::{DateTime, Utc};

use crate::domains::runtime_ingestion::{RuntimeQueueIsolationSummary, RuntimeQueueWaitingReason};

#[derive(Debug, Clone)]
pub struct QueueIsolationService {
    total_worker_slots: usize,
    minimum_slice_capacity: usize,
}

impl Default for QueueIsolationService {
    fn default() -> Self {
        Self::new(4, 1)
    }
}

impl QueueIsolationService {
    #[must_use]
    pub fn new(total_worker_slots: usize, minimum_slice_capacity: usize) -> Self {
        Self {
            total_worker_slots: total_worker_slots.max(1),
            minimum_slice_capacity: minimum_slice_capacity.max(1),
        }
    }

    #[must_use]
    pub fn summarize(
        &self,
        queued_count: usize,
        processing_count: usize,
        _workspace_processing_count: usize,
        global_processing_count: usize,
        last_claimed_at: Option<DateTime<Utc>>,
        last_progress_at: Option<DateTime<Utc>>,
        waiting_reason_hint: Option<RuntimeQueueWaitingReason>,
    ) -> RuntimeQueueIsolationSummary {
        let general_capacity_count = self.general_capacity_count();
        let isolated_capacity_count = if queued_count == 0 && processing_count == 0 {
            0
        } else {
            processing_count.max(self.minimum_slice_capacity.min(self.total_worker_slots))
        };
        let available_capacity_count =
            self.total_worker_slots.saturating_sub(global_processing_count);
        let waiting_reason = waiting_reason_hint.unwrap_or_else(|| {
            if processing_count > 0 || queued_count == 0 {
                RuntimeQueueWaitingReason::OrdinaryBacklog
            } else if global_processing_count >= general_capacity_count && processing_count == 0 {
                RuntimeQueueWaitingReason::IsolatedCapacityWait
            } else {
                RuntimeQueueWaitingReason::OrdinaryBacklog
            }
        });

        RuntimeQueueIsolationSummary {
            waiting_reason,
            queued_count,
            processing_count,
            isolated_capacity_count,
            available_capacity_count,
            last_claimed_at,
            last_progress_at,
        }
    }

    #[must_use]
    pub fn general_capacity_count(&self) -> usize {
        if self.total_worker_slots <= self.minimum_slice_capacity {
            self.total_worker_slots
        } else {
            self.total_worker_slots - self.minimum_slice_capacity
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_isolated_wait_when_all_capacity_is_busy_elsewhere() {
        let service = QueueIsolationService::new(4, 1);
        let summary = service.summarize(3, 0, 2, 4, None, None, None);

        assert_eq!(summary.waiting_reason, RuntimeQueueWaitingReason::IsolatedCapacityWait);
        assert_eq!(summary.isolated_capacity_count, 1);
        assert_eq!(summary.available_capacity_count, 0);
    }

    #[test]
    fn derives_isolated_wait_when_general_capacity_is_consumed() {
        let service = QueueIsolationService::new(4, 1);
        let summary = service.summarize(2, 0, 1, 3, None, None, None);

        assert_eq!(summary.waiting_reason, RuntimeQueueWaitingReason::IsolatedCapacityWait);
        assert_eq!(service.general_capacity_count(), 3);
        assert_eq!(summary.available_capacity_count, 1);
    }

    #[test]
    fn keeps_hint_for_degraded_waiting_states() {
        let service = QueueIsolationService::default();
        let summary =
            service.summarize(1, 0, 0, 0, None, None, Some(RuntimeQueueWaitingReason::Degraded));

        assert_eq!(summary.waiting_reason, RuntimeQueueWaitingReason::Degraded);
    }
}
