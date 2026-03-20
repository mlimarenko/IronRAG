use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domains::runtime_ingestion::{
    RuntimeCollectionResidualReason, RuntimeCollectionTerminalOutcome,
    RuntimeCollectionTerminalState,
};

#[derive(Debug, Clone, Default)]
pub struct TerminalSettlementService;

impl TerminalSettlementService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn summarize(
        &self,
        queued_count: usize,
        processing_count: usize,
        pending_graph_count: usize,
        failed_document_count: usize,
        missing_stage_count: i32,
        residual_reason: Option<RuntimeCollectionResidualReason>,
        live_total_estimated_cost: Option<Decimal>,
        settled_total_estimated_cost: Option<Decimal>,
        missing_total_estimated_cost: Option<Decimal>,
        currency: Option<String>,
        settled_at: Option<DateTime<Utc>>,
        last_transition_at: Option<DateTime<Utc>>,
    ) -> RuntimeCollectionTerminalOutcome {
        let terminal_state = if queued_count > 0 || processing_count > 0 || pending_graph_count > 0
        {
            RuntimeCollectionTerminalState::LiveInFlight
        } else if residual_reason.is_some() || failed_document_count > 0 || missing_stage_count > 0
        {
            RuntimeCollectionTerminalState::FailedWithResidualWork
        } else {
            RuntimeCollectionTerminalState::FullySettled
        };

        RuntimeCollectionTerminalOutcome {
            terminal_state: terminal_state.clone(),
            residual_reason: if matches!(
                terminal_state,
                RuntimeCollectionTerminalState::FailedWithResidualWork
            ) {
                Self::canonical_residual_reason(
                    residual_reason,
                    failed_document_count,
                    missing_stage_count,
                )
            } else {
                None
            },
            queued_count,
            processing_count,
            pending_graph_count,
            failed_document_count,
            live_total_estimated_cost,
            settled_total_estimated_cost,
            missing_total_estimated_cost,
            currency,
            settled_at: if matches!(terminal_state, RuntimeCollectionTerminalState::FullySettled) {
                settled_at.or_else(|| Some(Utc::now()))
            } else {
                settled_at
            },
            last_transition_at: last_transition_at.unwrap_or_else(Utc::now),
        }
    }

    #[must_use]
    fn canonical_residual_reason(
        residual_reason: Option<RuntimeCollectionResidualReason>,
        failed_document_count: usize,
        missing_stage_count: i32,
    ) -> Option<RuntimeCollectionResidualReason> {
        residual_reason.or_else(|| {
            if missing_stage_count > 0 {
                Some(RuntimeCollectionResidualReason::SettlementRefreshFailed)
            } else if failed_document_count > 0 {
                Some(RuntimeCollectionResidualReason::Unknown)
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_live_when_any_queue_or_processing_work_remains() {
        let service = TerminalSettlementService::new();
        let outcome = service.summarize(1, 0, 0, 0, 0, None, None, None, None, None, None, None);

        assert_eq!(outcome.terminal_state, RuntimeCollectionTerminalState::LiveInFlight);
        assert!(outcome.residual_reason.is_none());
    }

    #[test]
    fn marks_failed_with_residual_when_missing_or_failed_work_remains() {
        let service = TerminalSettlementService::new();
        let outcome = service.summarize(
            0,
            0,
            0,
            2,
            1,
            Some(RuntimeCollectionResidualReason::ProviderFailure),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        assert_eq!(outcome.terminal_state, RuntimeCollectionTerminalState::FailedWithResidualWork);
        assert_eq!(outcome.residual_reason, Some(RuntimeCollectionResidualReason::ProviderFailure));
    }

    #[test]
    fn falls_back_to_explicit_unknown_when_failed_work_has_no_specific_class() {
        let service = TerminalSettlementService::new();
        let outcome = service.summarize(0, 0, 0, 1, 0, None, None, None, None, None, None, None);

        assert_eq!(outcome.terminal_state, RuntimeCollectionTerminalState::FailedWithResidualWork);
        assert_eq!(outcome.residual_reason, Some(RuntimeCollectionResidualReason::Unknown));
    }
}
