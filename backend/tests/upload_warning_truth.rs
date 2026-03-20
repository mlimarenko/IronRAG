use rust_decimal::Decimal;

use rustrag_backend::{
    domains::runtime_ingestion::{
        RuntimeCollectionProgressState, RuntimeCollectionSettlementSummary,
        RuntimeOperatorWarningKind, RuntimeQueueIsolationSummary, RuntimeQueueWaitingReason,
    },
    services::operator_warning::OperatorWarningService,
    shared::file_extract::classify_multipart_file_body_error,
};

#[test]
fn multipart_stream_and_size_limit_rejections_stay_specific() {
    let stream_failure = classify_multipart_file_body_error(
        Some("report.pdf"),
        Some("application/pdf"),
        4,
        "failed to read stream to end",
    );
    assert_eq!(stream_failure.error_kind(), "multipart_stream_failure");
    assert_eq!(
        stream_failure.details().rejection_kind.as_deref(),
        Some("multipart_stream_failure")
    );

    let size_limit = classify_multipart_file_body_error(
        Some("large.pdf"),
        Some("application/pdf"),
        4,
        "field size exceeded",
    );
    assert_eq!(size_limit.error_kind(), "upload_limit_exceeded");
    assert_eq!(size_limit.details().rejection_kind.as_deref(), Some("upload_limit_exceeded"));
}

#[test]
fn backlog_and_live_accounting_stay_informational_while_real_risks_stay_degraded() {
    let service = OperatorWarningService::new();
    let warnings = service.build_collection_warnings(
        Some(&RuntimeQueueIsolationSummary {
            waiting_reason: RuntimeQueueWaitingReason::OrdinaryBacklog,
            queued_count: 6,
            processing_count: 2,
            isolated_capacity_count: 1,
            available_capacity_count: 0,
            last_claimed_at: None,
            last_progress_at: None,
        }),
        &RuntimeCollectionSettlementSummary {
            progress_state: RuntimeCollectionProgressState::LiveInFlight,
            live_total_estimated_cost: Some(Decimal::new(42, 2)),
            settled_total_estimated_cost: Some(Decimal::new(11, 2)),
            missing_total_estimated_cost: Some(Decimal::ZERO),
            currency: Some("USD".to_string()),
            is_fully_settled: false,
            settled_at: None,
        },
        1,
        2,
        1,
        1,
    );

    assert!(warnings.iter().any(|warning| {
        warning.warning_kind == RuntimeOperatorWarningKind::OrdinaryBacklog && !warning.is_degraded
    }));
    assert!(warnings.iter().any(|warning| {
        warning.warning_kind == RuntimeOperatorWarningKind::InFlightAccounting
            && !warning.is_degraded
    }));
    assert!(warnings.iter().any(|warning| {
        warning.warning_kind == RuntimeOperatorWarningKind::MissingAccounting && warning.is_degraded
    }));
    assert!(warnings.iter().any(|warning| {
        warning.warning_kind == RuntimeOperatorWarningKind::FailedWork && warning.is_degraded
    }));
    assert!(warnings.iter().any(|warning| {
        warning.warning_kind == RuntimeOperatorWarningKind::DegradedExtraction
            && warning.is_degraded
    }));
}
