use crate::domains::runtime_ingestion::{
    RuntimeCollectionSettlementSummary, RuntimeCollectionWarning, RuntimeOperatorWarningKind,
    RuntimeOperatorWarningScope, RuntimeQueueIsolationSummary, RuntimeQueueWaitingReason,
};

#[derive(Debug, Clone, Default)]
pub struct OperatorWarningService;

impl OperatorWarningService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn build_collection_warnings(
        &self,
        queue_isolation: Option<&RuntimeQueueIsolationSummary>,
        settlement: &RuntimeCollectionSettlementSummary,
        failed_count: usize,
        missing_stage_count: i32,
        stalled_count: usize,
        degraded_extraction_count: usize,
    ) -> Vec<RuntimeCollectionWarning> {
        let mut warnings = Vec::new();

        if let Some(queue_isolation) = queue_isolation {
            match queue_isolation.waiting_reason {
                RuntimeQueueWaitingReason::OrdinaryBacklog if queue_isolation.queued_count > 0 => {
                    warnings.push(RuntimeCollectionWarning {
                        warning_kind: RuntimeOperatorWarningKind::OrdinaryBacklog,
                        warning_scope: RuntimeOperatorWarningScope::Collection,
                        warning_message: "Work remains queued behind ordinary backlog.".to_string(),
                        is_degraded: false,
                    });
                }
                RuntimeQueueWaitingReason::IsolatedCapacityWait => {
                    warnings.push(RuntimeCollectionWarning {
                        warning_kind: RuntimeOperatorWarningKind::IsolatedCapacityWait,
                        warning_scope: RuntimeOperatorWarningScope::Collection,
                        warning_message:
                            "This library is waiting for its isolated execution capacity."
                                .to_string(),
                        is_degraded: false,
                    });
                }
                RuntimeQueueWaitingReason::Blocked => warnings.push(RuntimeCollectionWarning {
                    warning_kind: RuntimeOperatorWarningKind::LivenessLoss,
                    warning_scope: RuntimeOperatorWarningScope::Collection,
                    warning_message: "The queue slice is blocked and requires operator attention."
                        .to_string(),
                    is_degraded: true,
                }),
                RuntimeQueueWaitingReason::Degraded => warnings.push(RuntimeCollectionWarning {
                    warning_kind: RuntimeOperatorWarningKind::LivenessLoss,
                    warning_scope: RuntimeOperatorWarningScope::Collection,
                    warning_message: "The queue slice has lost visible progress.".to_string(),
                    is_degraded: true,
                }),
                RuntimeQueueWaitingReason::OrdinaryBacklog => {}
            }
        }

        if settlement.live_total_estimated_cost.is_some() && !settlement.is_fully_settled {
            warnings.push(RuntimeCollectionWarning {
                warning_kind: RuntimeOperatorWarningKind::InFlightAccounting,
                warning_scope: RuntimeOperatorWarningScope::Collection,
                warning_message: "Live in-flight provider work is visible and still settling."
                    .to_string(),
                is_degraded: false,
            });
        }

        if missing_stage_count > 0 || settlement.missing_total_estimated_cost.is_some() {
            warnings.push(RuntimeCollectionWarning {
                warning_kind: RuntimeOperatorWarningKind::MissingAccounting,
                warning_scope: RuntimeOperatorWarningScope::Collection,
                warning_message: "Some provider work is still missing settled accounting."
                    .to_string(),
                is_degraded: true,
            });
        }

        if stalled_count > 0 {
            warnings.push(RuntimeCollectionWarning {
                warning_kind: RuntimeOperatorWarningKind::LivenessLoss,
                warning_scope: RuntimeOperatorWarningScope::Collection,
                warning_message: "One or more documents lost visible progress.".to_string(),
                is_degraded: true,
            });
        }

        if failed_count > 0 {
            warnings.push(RuntimeCollectionWarning {
                warning_kind: RuntimeOperatorWarningKind::FailedWork,
                warning_scope: RuntimeOperatorWarningScope::Collection,
                warning_message: "One or more documents failed and need review.".to_string(),
                is_degraded: true,
            });
        }

        if degraded_extraction_count > 0 {
            warnings.push(RuntimeCollectionWarning {
                warning_kind: RuntimeOperatorWarningKind::DegradedExtraction,
                warning_scope: RuntimeOperatorWarningScope::Collection,
                warning_message:
                    "One or more documents finished without a complete graph extraction result."
                        .to_string(),
                is_degraded: true,
            });
        }

        warnings
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use crate::domains::runtime_ingestion::RuntimeCollectionProgressState;

    #[test]
    fn backlog_warning_stays_informational() {
        let service = OperatorWarningService::new();
        let warnings = service.build_collection_warnings(
            Some(&RuntimeQueueIsolationSummary {
                waiting_reason: RuntimeQueueWaitingReason::OrdinaryBacklog,
                queued_count: 3,
                processing_count: 0,
                isolated_capacity_count: 1,
                available_capacity_count: 0,
                last_claimed_at: None,
                last_progress_at: None,
            }),
            &RuntimeCollectionSettlementSummary {
                progress_state: RuntimeCollectionProgressState::LiveInFlight,
                live_total_estimated_cost: Some(Decimal::new(42, 2)),
                settled_total_estimated_cost: None,
                missing_total_estimated_cost: None,
                currency: Some("USD".to_string()),
                is_fully_settled: false,
                settled_at: None,
            },
            0,
            0,
            0,
            0,
        );

        assert!(warnings.iter().any(|warning| !warning.is_degraded));
        assert!(
            warnings.iter().all(
                |warning| warning.warning_kind != RuntimeOperatorWarningKind::MissingAccounting
            )
        );
    }
}
