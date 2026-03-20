use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domains::runtime_graph::{
    RuntimeGraphDiagnosticsSnapshot, RuntimeGraphProjectionHealth, RuntimeGraphWriteFailureKind,
};

#[derive(Debug, Clone)]
pub struct GraphDiagnosticsSnapshotService {
    stale_after_seconds: u64,
}

impl Default for GraphDiagnosticsSnapshotService {
    fn default() -> Self {
        Self::new(30)
    }
}

impl GraphDiagnosticsSnapshotService {
    #[must_use]
    pub fn new(stale_after_seconds: u64) -> Self {
        Self { stale_after_seconds: stale_after_seconds.max(1) }
    }

    #[must_use]
    pub fn stale_after_seconds(&self) -> u64 {
        self.stale_after_seconds
    }

    #[must_use]
    pub fn persist_interval_seconds(&self) -> u64 {
        (self.stale_after_seconds / 3).clamp(5, self.stale_after_seconds)
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn summarize(
        &self,
        library_id: Uuid,
        active_projection_count: usize,
        retrying_projection_count: usize,
        failed_projection_count: usize,
        pending_node_write_count: usize,
        pending_edge_write_count: usize,
        last_projection_failure_kind: Option<RuntimeGraphWriteFailureKind>,
        last_projection_failure_at: Option<DateTime<Utc>>,
        is_runtime_readable: bool,
    ) -> RuntimeGraphDiagnosticsSnapshot {
        let projection_health = if failed_projection_count > 0 {
            RuntimeGraphProjectionHealth::Failed
        } else if retrying_projection_count > 0 {
            RuntimeGraphProjectionHealth::RetryingContention
        } else if active_projection_count > 0
            || pending_node_write_count > 0
            || pending_edge_write_count > 0
        {
            RuntimeGraphProjectionHealth::Degraded
        } else {
            RuntimeGraphProjectionHealth::Healthy
        };

        RuntimeGraphDiagnosticsSnapshot {
            library_id,
            snapshot_at: Utc::now(),
            projection_health,
            active_projection_count,
            retrying_projection_count,
            failed_projection_count,
            pending_node_write_count,
            pending_edge_write_count,
            last_projection_failure_kind,
            last_projection_failure_at,
            is_runtime_readable,
        }
    }

    #[must_use]
    pub fn should_persist(
        &self,
        previous: Option<&RuntimeGraphDiagnosticsSnapshot>,
        next: &RuntimeGraphDiagnosticsSnapshot,
    ) -> bool {
        let Some(previous) = previous else {
            return true;
        };

        if previous.library_id != next.library_id
            || previous.projection_health != next.projection_health
            || previous.active_projection_count != next.active_projection_count
            || previous.retrying_projection_count != next.retrying_projection_count
            || previous.failed_projection_count != next.failed_projection_count
            || previous.pending_node_write_count != next.pending_node_write_count
            || previous.pending_edge_write_count != next.pending_edge_write_count
            || previous.last_projection_failure_kind != next.last_projection_failure_kind
            || previous.last_projection_failure_at != next.last_projection_failure_at
            || previous.is_runtime_readable != next.is_runtime_readable
        {
            return true;
        }

        (next.snapshot_at - previous.snapshot_at).num_seconds()
            >= i64::try_from(self.persist_interval_seconds()).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    #[test]
    fn suppresses_identical_snapshot_writes_inside_persist_window() {
        let service = GraphDiagnosticsSnapshotService::new(30);
        let library_id = Uuid::now_v7();
        let previous = RuntimeGraphDiagnosticsSnapshot {
            library_id,
            snapshot_at: Utc::now(),
            projection_health: RuntimeGraphProjectionHealth::Healthy,
            active_projection_count: 0,
            retrying_projection_count: 0,
            failed_projection_count: 0,
            pending_node_write_count: 0,
            pending_edge_write_count: 0,
            last_projection_failure_kind: None,
            last_projection_failure_at: None,
            is_runtime_readable: true,
        };
        let next = RuntimeGraphDiagnosticsSnapshot {
            snapshot_at: previous.snapshot_at + Duration::seconds(3),
            ..previous.clone()
        };

        assert!(!service.should_persist(Some(&previous), &next));
    }

    #[test]
    fn persists_when_projection_state_changes_even_inside_window() {
        let service = GraphDiagnosticsSnapshotService::new(30);
        let library_id = Uuid::now_v7();
        let previous = RuntimeGraphDiagnosticsSnapshot {
            library_id,
            snapshot_at: Utc::now(),
            projection_health: RuntimeGraphProjectionHealth::Healthy,
            active_projection_count: 0,
            retrying_projection_count: 0,
            failed_projection_count: 0,
            pending_node_write_count: 0,
            pending_edge_write_count: 0,
            last_projection_failure_kind: None,
            last_projection_failure_at: None,
            is_runtime_readable: true,
        };
        let next = RuntimeGraphDiagnosticsSnapshot {
            projection_health: RuntimeGraphProjectionHealth::RetryingContention,
            retrying_projection_count: 1,
            snapshot_at: previous.snapshot_at + Duration::seconds(1),
            ..previous.clone()
        };

        assert!(service.should_persist(Some(&previous), &next));
    }
}
