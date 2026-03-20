use std::{fs, path::PathBuf};

fn load_contract() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts").join("rustrag.openapi.yaml");
    let contract = fs::read_to_string(&path);
    assert!(contract.is_ok(), "expected OpenAPI contract at {}", path.display());
    contract.unwrap_or_default()
}

#[test]
fn resolve_settle_blockers_contract_keeps_terminal_and_graph_health_schemas() {
    let contract = load_contract();

    assert!(contract.contains("UiDocumentTerminalOutcomeSummary:"));
    assert!(contract.contains("UiDocumentGraphHealthSummary:"));
    assert!(contract.contains("UiDocumentsWorkspace:"));
    assert!(contract.contains("RuntimeTerminalOutcomeResponse:"));
    assert!(contract.contains("RuntimeGraphHealthResponse:"));
    assert!(contract.contains("RuntimeGraphDiagnosticsResponse:"));
    assert!(contract.contains("providerFailureCounts:"));
    assert!(contract.contains("recentProviderFailureClasses:"));
    assert!(contract.contains("residualFailureCounts:"));
}

#[test]
fn resolve_settle_blockers_contract_keeps_provider_failure_and_upload_limit_taxonomy() {
    let contract = load_contract();

    assert!(contract.contains("UiDocumentProviderFailureSummary:"));
    assert!(contract.contains("RuntimeProviderFailureResponse:"));
    assert!(contract.contains("upload_limit_exceeded"));
    assert!(contract.contains("providerFailure:"));
    assert!(contract.contains("terminalOutcome:"));
    assert!(contract.contains("graphHealth:"));
    assert!(contract.contains("upstream_protocol_failure"));
    assert!(contract.contains("upstream_timeout"));
    assert!(contract.contains("recovered_after_retry"));
    assert!(contract.contains("requestShapeKey"));
    assert!(contract.contains("upstreamStatus"));
}

#[test]
fn resolve_settle_blockers_contract_keeps_workspace_and_size_limit_examples() {
    let contract = load_contract();

    assert!(contract.contains("UiDocumentsWorkspacePrimarySummary:"));
    assert!(contract.contains("uploadLimitMb: 50"));
    assert!(contract.contains("rejectionCause: upload_limit_exceeded"));
    assert!(contract.contains("terminalState: failed_with_residual_work"));
    assert!(contract.contains("projectionHealth: retrying_contention"));
}
