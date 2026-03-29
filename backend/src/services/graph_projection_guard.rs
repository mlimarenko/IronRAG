use crate::infra::arangodb::graph_store::GraphViewWriteError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphWriteFailureDecision {
    RetryContention,
    FailTerminal,
}

#[derive(Debug, Clone)]
pub struct GraphWriteGuardService {
    max_retry_count: usize,
}

impl Default for GraphWriteGuardService {
    fn default() -> Self {
        Self::new(3)
    }
}

impl GraphWriteGuardService {
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
        error: &GraphViewWriteError,
        next_retry_count: usize,
    ) -> GraphWriteFailureDecision {
        match error {
            GraphViewWriteError::GraphWriteContention { .. }
                if next_retry_count < self.max_retry_count =>
            {
                GraphWriteFailureDecision::RetryContention
            }
            GraphViewWriteError::GraphWriteContention { .. }
            | GraphViewWriteError::GraphPersistenceIntegrity { .. }
            | GraphViewWriteError::GraphWriteFailure { .. } => {
                GraphWriteFailureDecision::FailTerminal
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_retryable_contention_strings() {
        let service = GraphWriteGuardService::default();

        assert!(service.is_retryable_contention("graph write deadlock detected"));
        assert!(!service.is_retryable_contention("validation failed"));
    }

    #[test]
    fn keeps_retryable_contention_on_retry_path_before_exhaustion() {
        let service = GraphWriteGuardService::new(3);
        let decision = service.classify_write_error(
            &GraphViewWriteError::GraphWriteContention { message: "deadlock".to_string() },
            1,
        );

        assert_eq!(decision, GraphWriteFailureDecision::RetryContention);
    }

    #[test]
    fn classifies_exhausted_contention_explicitly() {
        let service = GraphWriteGuardService::new(3);
        let decision = service.classify_write_error(
            &GraphViewWriteError::GraphWriteContention { message: "deadlock".to_string() },
            3,
        );

        assert_eq!(decision, GraphWriteFailureDecision::FailTerminal);
    }
}
