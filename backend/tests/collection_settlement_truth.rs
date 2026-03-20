mod pipeline_hardening_support;

use chrono::Utc;
use rust_decimal::Decimal;

use pipeline_hardening_support::sample_settlement_row;
use rustrag_backend::{
    domains::runtime_ingestion::{
        RuntimeAccountingTruthStatus, RuntimeCollectionProgressState,
        RuntimeCollectionTerminalOutcome, RuntimeCollectionTerminalState,
    },
    infra::repositories,
    services::collection_settlement::CollectionSettlementService,
};

#[test]
fn settlement_service_distinguishes_fully_settled_from_residual_work() {
    let service = CollectionSettlementService::new();
    let fully_settled_terminal = RuntimeCollectionTerminalOutcome {
        terminal_state: RuntimeCollectionTerminalState::FullySettled,
        residual_reason: None,
        queued_count: 0,
        processing_count: 0,
        pending_graph_count: 0,
        failed_document_count: 0,
        live_total_estimated_cost: None,
        settled_total_estimated_cost: Some(Decimal::new(1250, 2)),
        missing_total_estimated_cost: None,
        currency: Some("USD".to_string()),
        settled_at: None,
        last_transition_at: Utc::now(),
    };
    let residual_terminal = RuntimeCollectionTerminalOutcome {
        terminal_state: RuntimeCollectionTerminalState::FailedWithResidualWork,
        residual_reason: None,
        queued_count: 0,
        processing_count: 0,
        pending_graph_count: 1,
        failed_document_count: 0,
        live_total_estimated_cost: None,
        settled_total_estimated_cost: Some(Decimal::new(1250, 2)),
        missing_total_estimated_cost: None,
        currency: Some("USD".to_string()),
        settled_at: None,
        last_transition_at: Utc::now(),
    };

    let fully_settled = service.summarize(
        &fully_settled_terminal,
        None,
        Some(Decimal::new(1250, 2)),
        None,
        Some("USD".to_string()),
        0,
        0,
        RuntimeAccountingTruthStatus::Priced,
        None,
    );
    let residual_work = service.summarize(
        &residual_terminal,
        None,
        Some(Decimal::new(1250, 2)),
        None,
        Some("USD".to_string()),
        0,
        0,
        RuntimeAccountingTruthStatus::Priced,
        None,
    );

    assert_eq!(fully_settled.progress_state, RuntimeCollectionProgressState::FullySettled);
    assert!(fully_settled.is_fully_settled);
    assert!(fully_settled.settled_at.is_some());

    assert_eq!(
        residual_work.progress_state,
        RuntimeCollectionProgressState::FailedWithResidualWork
    );
    assert!(!residual_work.is_fully_settled);
    assert!(residual_work.settled_at.is_none());
}

#[test]
fn settlement_repository_keys_preserve_extended_snapshot_fields() {
    let row = sample_settlement_row();

    assert_eq!(row.queue_backlog_count, 7);
    assert_eq!(row.processing_backlog_count, 4);
    assert_eq!(row.prompt_tokens, 12_345);
    assert_eq!(row.completion_tokens, 2_345);
    assert_eq!(row.total_tokens, 14_690);
    assert_eq!(row.priced_stage_count, 24);
    assert_eq!(row.unpriced_stage_count, 2);
    assert_eq!(row.in_flight_stage_count, 3);
    assert_eq!(row.missing_stage_count, 1);
    assert_eq!(row.accounting_status, "in_flight_unsettled");
    assert_eq!(
        repositories::parse_runtime_collection_progress_state(Some(row.progress_state.as_str())),
        RuntimeCollectionProgressState::Settling
    );
    assert!(row.computed_at <= Utc::now());
}
