use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domains::runtime_graph::{
    RuntimeGraphDiagnosticsSnapshot, RuntimeGraphProjectionHealth, RuntimeGraphProjectionLockState,
    RuntimeGraphProjectionScope, RuntimeGraphProjectionWriteState, RuntimeGraphWriteFailureKind,
};
use crate::infra::graph_store::GraphProjectionWriteError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphProjectionFailureDecision {
    RetryContention,
    FailExplicitly(RuntimeGraphWriteFailureKind),
}

#[derive(Debug, Clone)]
pub struct GraphProjectionGuardService {
    max_retry_count: usize,
}

impl Default for GraphProjectionGuardService {
    fn default() -> Self {
        Self::new(3)
    }
}

impl GraphProjectionGuardService {
    #[must_use]
    pub fn new(max_retry_count: usize) -> Self {
        Self { max_retry_count: max_retry_count.max(1) }
    }

    #[must_use]
    pub fn max_retry_count(&self) -> usize {
        self.max_retry_count
    }

    #[must_use]
    pub fn is_retryable_contention(&self, message: &str) -> bool {
        let normalized = message.to_ascii_lowercase();
        normalized.contains("deadlock")
            || normalized.contains("lock")
            || normalized.contains("transient")
            || normalized.contains("concurrent")
    }

    #[must_use]
    pub fn classify_write_error(
        &self,
        error: &GraphProjectionWriteError,
        next_retry_count: usize,
    ) -> GraphProjectionFailureDecision {
        match error {
            GraphProjectionWriteError::ProjectionContention { .. }
                if next_retry_count < self.max_retry_count =>
            {
                GraphProjectionFailureDecision::RetryContention
            }
            GraphProjectionWriteError::ProjectionContention { .. } => {
                GraphProjectionFailureDecision::FailExplicitly(
                    RuntimeGraphWriteFailureKind::ProjectionContention,
                )
            }
            GraphProjectionWriteError::GraphPersistenceIntegrity { .. } => {
                GraphProjectionFailureDecision::FailExplicitly(
                    RuntimeGraphWriteFailureKind::GraphPersistenceIntegrity,
                )
            }
            GraphProjectionWriteError::ProjectionFailure { .. } => {
                GraphProjectionFailureDecision::FailExplicitly(
                    RuntimeGraphWriteFailureKind::ProjectionFailure,
                )
            }
        }
    }

    #[must_use]
    pub fn classify_health(
        &self,
        active_projection_count: usize,
        retrying_projection_count: usize,
        failed_projection_count: usize,
    ) -> RuntimeGraphProjectionHealth {
        if failed_projection_count > 0 {
            RuntimeGraphProjectionHealth::Failed
        } else if retrying_projection_count > 0 {
            RuntimeGraphProjectionHealth::RetryingContention
        } else if active_projection_count > 0 {
            RuntimeGraphProjectionHealth::Degraded
        } else {
            RuntimeGraphProjectionHealth::Healthy
        }
    }

    #[must_use]
    pub fn build_scope(
        &self,
        library_id: Uuid,
        scope_kind: impl Into<String>,
        attempt_no: i32,
        lock_state: RuntimeGraphProjectionLockState,
        write_state: RuntimeGraphProjectionWriteState,
        deadlock_retry_count: usize,
        started_at: DateTime<Utc>,
        finished_at: Option<DateTime<Utc>>,
        failure_kind: Option<RuntimeGraphWriteFailureKind>,
    ) -> RuntimeGraphProjectionScope {
        RuntimeGraphProjectionScope {
            id: Uuid::now_v7(),
            library_id,
            scope_kind: scope_kind.into(),
            attempt_no,
            lock_state,
            write_state,
            deadlock_retry_count,
            started_at,
            finished_at,
            failure_kind,
        }
    }

    #[must_use]
    pub fn empty_snapshot(&self, library_id: Uuid) -> RuntimeGraphDiagnosticsSnapshot {
        RuntimeGraphDiagnosticsSnapshot {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_retryable_contention_strings() {
        let service = GraphProjectionGuardService::default();

        assert!(service.is_retryable_contention("Neo4j deadlock detected"));
        assert!(!service.is_retryable_contention("validation failed"));
    }

    #[test]
    fn keeps_retryable_contention_on_retry_path_before_exhaustion() {
        let service = GraphProjectionGuardService::new(3);
        let decision = service.classify_write_error(
            &GraphProjectionWriteError::ProjectionContention { message: "deadlock".to_string() },
            1,
        );

        assert_eq!(decision, GraphProjectionFailureDecision::RetryContention);
    }

    #[test]
    fn classifies_exhausted_contention_explicitly() {
        let service = GraphProjectionGuardService::new(3);
        let decision = service.classify_write_error(
            &GraphProjectionWriteError::ProjectionContention { message: "deadlock".to_string() },
            3,
        );

        assert_eq!(
            decision,
            GraphProjectionFailureDecision::FailExplicitly(
                RuntimeGraphWriteFailureKind::ProjectionContention,
            )
        );
    }
}
