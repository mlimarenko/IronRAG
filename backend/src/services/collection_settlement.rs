use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domains::runtime_ingestion::{
    RuntimeAccountingTruthStatus, RuntimeCollectionProgressState,
    RuntimeCollectionSettlementSummary, RuntimeCollectionTerminalOutcome,
    RuntimeCollectionTerminalState,
};

#[derive(Debug, Clone, Default)]
pub struct CollectionSettlementService;

impl CollectionSettlementService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn summarize(
        &self,
        terminal_outcome: &RuntimeCollectionTerminalOutcome,
        live_total_estimated_cost: Option<Decimal>,
        settled_total_estimated_cost: Option<Decimal>,
        missing_total_estimated_cost: Option<Decimal>,
        currency: Option<String>,
        in_flight_stage_count: i32,
        missing_stage_count: i32,
        accounting_status: RuntimeAccountingTruthStatus,
        settled_at: Option<DateTime<Utc>>,
    ) -> RuntimeCollectionSettlementSummary {
        let progress_state = match terminal_outcome.terminal_state {
            RuntimeCollectionTerminalState::FailedWithResidualWork => {
                RuntimeCollectionProgressState::FailedWithResidualWork
            }
            RuntimeCollectionTerminalState::FullySettled => {
                RuntimeCollectionProgressState::FullySettled
            }
            RuntimeCollectionTerminalState::LiveInFlight
                if terminal_outcome.queued_count > 0
                    || terminal_outcome.processing_count > 0
                    || terminal_outcome.pending_graph_count > 0 =>
            {
                RuntimeCollectionProgressState::LiveInFlight
            }
            RuntimeCollectionTerminalState::LiveInFlight
                if in_flight_stage_count > 0
                    || missing_stage_count > 0
                    || matches!(
                        accounting_status,
                        RuntimeAccountingTruthStatus::InFlightUnsettled
                    ) =>
            {
                RuntimeCollectionProgressState::Settling
            }
            RuntimeCollectionTerminalState::LiveInFlight => {
                RuntimeCollectionProgressState::FullySettled
            }
        };
        RuntimeCollectionSettlementSummary {
            progress_state: progress_state.clone(),
            live_total_estimated_cost,
            settled_total_estimated_cost,
            missing_total_estimated_cost,
            currency,
            is_fully_settled: progress_state == RuntimeCollectionProgressState::FullySettled,
            settled_at: if progress_state == RuntimeCollectionProgressState::FullySettled {
                settled_at.or(Some(Utc::now()))
            } else {
                None
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_work_keeps_collection_in_flight() {
        let service = CollectionSettlementService::new();
        let terminal = RuntimeCollectionTerminalOutcome {
            terminal_state: RuntimeCollectionTerminalState::LiveInFlight,
            residual_reason: None,
            queued_count: 2,
            processing_count: 1,
            pending_graph_count: 0,
            failed_document_count: 0,
            live_total_estimated_cost: None,
            settled_total_estimated_cost: None,
            missing_total_estimated_cost: None,
            currency: None,
            settled_at: None,
            last_transition_at: Utc::now(),
        };

        assert_eq!(
            service
                .summarize(
                    &terminal,
                    None,
                    None,
                    None,
                    None,
                    1,
                    0,
                    RuntimeAccountingTruthStatus::InFlightUnsettled,
                    None
                )
                .progress_state,
            RuntimeCollectionProgressState::LiveInFlight
        );
    }

    #[test]
    fn zero_backlog_without_missing_or_failures_becomes_fully_settled() {
        let service = CollectionSettlementService::new();
        let terminal = RuntimeCollectionTerminalOutcome {
            terminal_state: RuntimeCollectionTerminalState::FullySettled,
            residual_reason: None,
            queued_count: 0,
            processing_count: 0,
            pending_graph_count: 0,
            failed_document_count: 0,
            live_total_estimated_cost: None,
            settled_total_estimated_cost: None,
            missing_total_estimated_cost: None,
            currency: None,
            settled_at: None,
            last_transition_at: Utc::now(),
        };

        assert_eq!(
            service
                .summarize(
                    &terminal,
                    None,
                    None,
                    None,
                    None,
                    0,
                    0,
                    RuntimeAccountingTruthStatus::Priced,
                    None,
                )
                .progress_state,
            RuntimeCollectionProgressState::FullySettled
        );
    }

    #[test]
    fn degraded_extraction_blocks_fully_settled_state() {
        let service = CollectionSettlementService::new();
        let terminal = RuntimeCollectionTerminalOutcome {
            terminal_state: RuntimeCollectionTerminalState::FailedWithResidualWork,
            residual_reason: None,
            queued_count: 0,
            processing_count: 0,
            pending_graph_count: 1,
            failed_document_count: 0,
            live_total_estimated_cost: None,
            settled_total_estimated_cost: None,
            missing_total_estimated_cost: None,
            currency: None,
            settled_at: None,
            last_transition_at: Utc::now(),
        };

        assert_eq!(
            service
                .summarize(
                    &terminal,
                    None,
                    None,
                    None,
                    None,
                    0,
                    0,
                    RuntimeAccountingTruthStatus::Priced,
                    None,
                )
                .progress_state,
            RuntimeCollectionProgressState::FailedWithResidualWork
        );
    }
}
