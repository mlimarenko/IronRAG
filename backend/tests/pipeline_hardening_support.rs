#![allow(dead_code)]

use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use rustrag_backend::{
    domains::runtime_ingestion::{
        RuntimeAccountingTruthStatus, RuntimeCollectionProgressState,
        RuntimeCollectionSettlementSummary, RuntimeCollectionWarning, RuntimeOperatorWarningKind,
        RuntimeOperatorWarningScope, RuntimeQueueIsolationSummary, RuntimeQueueWaitingReason,
    },
    infra::repositories::{
        RuntimeCollectionSettlementRollupInput, RuntimeCollectionSettlementRow,
        RuntimeCollectionWarningRow, RuntimeLibraryQueueSliceRow,
    },
};

#[must_use]
pub fn sample_queue_slice_row() -> RuntimeLibraryQueueSliceRow {
    RuntimeLibraryQueueSliceRow {
        workspace_id: Uuid::nil(),
        project_id: Uuid::nil(),
        queued_count: 3,
        processing_count: 0,
        workspace_processing_count: 2,
        global_processing_count: 4,
        last_claimed_at: Some(Utc::now()),
        last_progress_at: Some(Utc::now()),
        waiting_reason: Some("isolated_capacity_wait".to_string()),
    }
}

#[must_use]
pub fn sample_queue_isolation_summary() -> RuntimeQueueIsolationSummary {
    RuntimeQueueIsolationSummary {
        waiting_reason: RuntimeQueueWaitingReason::IsolatedCapacityWait,
        queued_count: 3,
        processing_count: 0,
        isolated_capacity_count: 1,
        available_capacity_count: 0,
        last_claimed_at: Some(Utc::now()),
        last_progress_at: Some(Utc::now()),
    }
}

#[must_use]
pub fn sample_settlement_summary() -> RuntimeCollectionSettlementSummary {
    RuntimeCollectionSettlementSummary {
        progress_state: RuntimeCollectionProgressState::Settling,
        live_total_estimated_cost: Some(Decimal::new(1234, 4)),
        settled_total_estimated_cost: Some(Decimal::new(99, 2)),
        missing_total_estimated_cost: Some(Decimal::ZERO),
        currency: Some("USD".to_string()),
        is_fully_settled: false,
        settled_at: None,
    }
}

#[must_use]
pub fn sample_settlement_row() -> RuntimeCollectionSettlementRow {
    RuntimeCollectionSettlementRow {
        project_id: Uuid::nil(),
        progress_state: "settling".to_string(),
        terminal_state: "live_in_flight".to_string(),
        terminal_transition_at: Utc::now(),
        residual_reason: None,
        document_count: 50,
        accepted_count: 50,
        content_extracted_count: 40,
        chunked_count: 38,
        embedded_count: 35,
        graph_active_count: 12,
        graph_ready_count: 20,
        pending_graph_count: 1,
        ready_count: 18,
        failed_count: 1,
        queue_backlog_count: 7,
        processing_backlog_count: 4,
        live_total_estimated_cost: Some(Decimal::new(1234, 4)),
        settled_total_estimated_cost: Some(Decimal::new(4567, 4)),
        missing_total_estimated_cost: Some(Decimal::ZERO),
        currency: Some("USD".to_string()),
        prompt_tokens: 12_345,
        completion_tokens: 2_345,
        total_tokens: 14_690,
        priced_stage_count: 24,
        unpriced_stage_count: 2,
        in_flight_stage_count: 3,
        missing_stage_count: 1,
        accounting_status: "in_flight_unsettled".to_string(),
        is_fully_settled: false,
        settled_at: None,
        computed_at: Utc::now(),
    }
}

#[must_use]
pub fn sample_warning() -> RuntimeCollectionWarning {
    RuntimeCollectionWarning {
        warning_kind: RuntimeOperatorWarningKind::OrdinaryBacklog,
        warning_scope: RuntimeOperatorWarningScope::Collection,
        warning_message: "Work remains queued behind ordinary backlog.".to_string(),
        is_degraded: false,
    }
}

#[must_use]
pub fn sample_warning_row() -> RuntimeCollectionWarningRow {
    RuntimeCollectionWarningRow {
        project_id: Uuid::nil(),
        warning_kind: "ordinary_backlog".to_string(),
        warning_scope: "collection".to_string(),
        warning_message: "Work remains queued behind ordinary backlog.".to_string(),
        is_degraded: false,
        computed_at: Utc::now(),
    }
}

#[must_use]
pub fn sample_settlement_rollup_input() -> RuntimeCollectionSettlementRollupInput {
    RuntimeCollectionSettlementRollupInput {
        scope_kind: "stage".to_string(),
        scope_key: "extracting_graph".to_string(),
        queued_count: 4,
        processing_count: 2,
        completed_count: 18,
        failed_count: 1,
        document_count: 20,
        ready_count: 0,
        ready_no_graph_count: 0,
        content_extracted_count: 0,
        chunked_count: 0,
        embedded_count: 0,
        graph_active_count: 0,
        graph_ready_count: 0,
        live_estimated_cost: Some(Decimal::new(75, 2)),
        settled_estimated_cost: Some(Decimal::new(420, 2)),
        missing_estimated_cost: Some(Decimal::ZERO),
        currency: Some("USD".to_string()),
        avg_elapsed_ms: Some(72_680),
        max_elapsed_ms: Some(178_591),
        bottleneck_stage: None,
        bottleneck_avg_elapsed_ms: None,
        bottleneck_max_elapsed_ms: None,
        prompt_tokens: 8_000,
        completion_tokens: 1_200,
        total_tokens: 9_200,
        accounting_status: "in_flight_unsettled".to_string(),
        bottleneck_rank: Some(1),
        is_primary_bottleneck: true,
    }
}

#[test]
fn support_builders_produce_non_empty_runtime_truth() {
    let queue = sample_queue_isolation_summary();
    let settlement = sample_settlement_summary();

    assert_eq!(queue.waiting_reason, RuntimeQueueWaitingReason::IsolatedCapacityWait);
    assert_eq!(settlement.progress_state, RuntimeCollectionProgressState::Settling);
    assert_eq!(RuntimeAccountingTruthStatus::Unpriced, RuntimeAccountingTruthStatus::Unpriced);
}
