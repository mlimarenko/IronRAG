use std::{fs, path::PathBuf};

fn load_contract() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts").join("rustrag.openapi.yaml");
    let contract = fs::read_to_string(&path);
    assert!(contract.is_ok(), "expected OpenAPI contract at {}", path.display());
    contract.unwrap_or_default()
}

#[test]
fn pipeline_hardening_contract_keeps_queue_and_settlement_truth() {
    let contract = load_contract();

    assert!(contract.contains("RuntimeQueueIsolationSummaryResponse:"));
    assert!(contract.contains("waitingReason:"));
    assert!(contract.contains("isolated_capacity_wait"));
    assert!(contract.contains("RuntimeCollectionDiagnosticsResponse:"));
    assert!(contract.contains("graphThroughput:"));
    assert!(contract.contains("RuntimeCollectionSettlementResponse:"));
    assert!(contract.contains("failed_with_residual_work"));
    assert!(contract.contains("settledAt:"));
    assert!(contract.contains("inFlightStageCount:"));
}

#[test]
fn pipeline_hardening_contract_keeps_upload_and_warning_payloads() {
    let contract = load_contract();

    assert!(contract.contains("RuntimeUploadRejectionDetails:"));
    assert!(contract.contains("rejectionKind:"));
    assert!(contract.contains("rejectionCause:"));
    assert!(contract.contains("operatorAction:"));
    assert!(contract.contains("RuntimeCollectionWarningResponse:"));
    assert!(contract.contains("ordinary_backlog"));
    assert!(contract.contains("isolated_capacity_wait"));
    assert!(contract.contains("missing_accounting"));
    assert!(contract.contains("degraded_extraction"));
}
