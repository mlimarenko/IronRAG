use crate::infra::knowledge_rows::GraphViewWriteError;

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
    pub const fn max_retry_count(&self) -> usize {
        self.max_retry_count
    }

    #[must_use]
    pub const fn classify_write_error(
        &self,
        error: &GraphViewWriteError,
        next_retry_count: usize,
    ) -> GraphWriteFailureDecision {
        if error.is_retryable_contention() && next_retry_count < self.max_retry_count {
            GraphWriteFailureDecision::RetryContention
        } else {
            GraphWriteFailureDecision::FailTerminal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_does_not_control_retryability() {
        let retryable =
            GraphViewWriteError::GraphWriteContention { message: "validation failed".to_string() };
        let terminal = GraphViewWriteError::GraphWriteFailure {
            message: "deadlock while writing".to_string(),
        };

        assert!(retryable.is_retryable_contention());
        assert!(!terminal.is_retryable_contention());
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
