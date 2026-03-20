mod resolve_settle_blockers_support;

use chrono::Utc;
use rust_decimal::Decimal;

use resolve_settle_blockers_support::sample_terminal_outcome;
use rustrag_backend::{
    domains::runtime_ingestion::{RuntimeCollectionResidualReason, RuntimeCollectionTerminalState},
    services::terminal_settlement::TerminalSettlementService,
};

#[test]
fn reducer_reaches_fully_settled_without_false_positive_residuals() {
    let service = TerminalSettlementService::new();

    let outcome = service.summarize(
        0,
        0,
        0,
        0,
        0,
        None,
        None,
        Some(Decimal::new(550, 2)),
        None,
        Some("USD".to_string()),
        None,
        None,
    );

    assert_eq!(outcome.terminal_state, RuntimeCollectionTerminalState::FullySettled);
    assert_eq!(outcome.residual_reason, None);
    assert!(outcome.settled_at.is_some());
}

#[test]
fn reducer_preserves_failed_with_residual_work_when_blockers_remain() {
    let service = TerminalSettlementService::new();
    let sample = sample_terminal_outcome();

    let outcome = service.summarize(
        sample.queued_count,
        sample.processing_count,
        0,
        sample.failed_document_count,
        0,
        Some(RuntimeCollectionResidualReason::ProjectionContention),
        sample.live_total_estimated_cost,
        sample.settled_total_estimated_cost,
        sample.missing_total_estimated_cost,
        sample.currency.clone(),
        sample.settled_at,
        Some(Utc::now()),
    );

    assert_eq!(outcome.terminal_state, RuntimeCollectionTerminalState::FailedWithResidualWork);
    assert_eq!(
        outcome.residual_reason,
        Some(RuntimeCollectionResidualReason::ProjectionContention)
    );
    assert_eq!(outcome.pending_graph_count, 0);
    assert_eq!(outcome.failed_document_count, sample.failed_document_count);
    assert!(outcome.settled_at.is_none());
}

#[test]
fn reducer_keeps_explicit_residual_reason_distinct_from_missing_accounting_fallback() {
    let service = TerminalSettlementService::new();

    let outcome = service.summarize(
        0,
        0,
        0,
        0,
        2,
        Some(RuntimeCollectionResidualReason::UploadLimitExceeded),
        None,
        None,
        None,
        Some("USD".to_string()),
        None,
        Some(Utc::now()),
    );

    assert_eq!(outcome.terminal_state, RuntimeCollectionTerminalState::FailedWithResidualWork);
    assert_eq!(outcome.residual_reason, Some(RuntimeCollectionResidualReason::UploadLimitExceeded));
}
