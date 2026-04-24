#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "greenfield_contracts.rs"]
mod greenfield_contracts;

use serde_json::json;
use uuid::Uuid;

use ironrag_backend::{
    interfaces::http::mcp::MCP_DIAGNOSTICS_TOOL_NAMES,
    mcp_types::{
        McpCancelWebIngestRunRequest, McpGetWebIngestRunRequest, McpListWebIngestRunPagesRequest,
        McpSubmitWebIngestRunRequest,
    },
    shared::web::ingest::{
        DEFAULT_WEB_CRAWL_DEPTH, DEFAULT_WEB_CRAWL_MAX_PAGES, WebClassificationReason,
        WebRunCounts, WebRunFailureCode, derive_terminal_run_state, validate_web_run_settings,
    },
};

#[test]
fn web_ingest_rest_surface_keeps_canonical_routes_and_runtime_defaults() {
    let contract = greenfield_contracts::load_openapi_contract_text();

    for path in [
        "/v1/content/web-runs:",
        "/v1/content/web-runs/{runId}:",
        "/v1/content/web-runs/{runId}/pages:",
        "/v1/content/web-runs/{runId}/cancel:",
    ] {
        assert!(contract.contains(path), "missing web ingest REST path `{path}`");
    }

    assert!(
        contract.contains("required: [libraryId, seedUrl, mode]"),
        "CreateWebIngestRunRequest must keep canonical required fields"
    );
    assert!(
        contract.contains("enum: [single_page, recursive_crawl]"),
        "web ingest mode enum must stay canonical in OpenAPI"
    );

    let single_page_defaults = validate_web_run_settings("single_page", None, Some(9), None)
        .expect("single page settings");
    assert_eq!(single_page_defaults.mode, "single_page");
    assert_eq!(single_page_defaults.boundary_policy, "same_host");
    assert_eq!(single_page_defaults.max_depth, 0);
    assert_eq!(single_page_defaults.max_pages, DEFAULT_WEB_CRAWL_MAX_PAGES);

    let recursive_defaults =
        validate_web_run_settings("recursive_crawl", None, None, None).expect("recursive settings");
    assert_eq!(recursive_defaults.mode, "recursive_crawl");
    assert_eq!(recursive_defaults.boundary_policy, "same_host");
    assert_eq!(recursive_defaults.max_depth, DEFAULT_WEB_CRAWL_DEPTH);
    assert_eq!(recursive_defaults.max_pages, DEFAULT_WEB_CRAWL_MAX_PAGES);
}

#[test]
fn web_ingest_contract_enums_cover_runtime_vocabulary_and_partial_count_grammar() {
    let contract = greenfield_contracts::load_openapi_contract_text();

    assert!(
        contract.contains("enum: [accepted, discovering, processing, completed, completed_partial, failed, canceled]"),
        "run state enum must keep completed_partial in OpenAPI"
    );
    assert!(
        contract.contains("[discovered, eligible, processed, queued, processing, duplicates, excluded, blocked, failed, canceled]"),
        "WebRunCounts must keep queued and processing grammar"
    );

    for reason in WebClassificationReason::ALL.map(WebClassificationReason::as_str) {
        assert!(
            contract.contains(&format!("- {reason}")),
            "missing classification reason `{reason}` in OpenAPI"
        );
    }

    for failure_code in WebRunFailureCode::ALL.map(WebRunFailureCode::as_str) {
        assert!(
            contract.contains(&format!("- {failure_code}")),
            "missing failure code `{failure_code}` in OpenAPI"
        );
    }

    let completed_partial = derive_terminal_run_state(&WebRunCounts {
        processed: 2,
        failed: 1,
        ..WebRunCounts::default()
    });
    assert_eq!(completed_partial.as_str(), "completed_partial");
}

#[test]
fn web_ingest_mcp_tool_vocabulary_and_request_fields_stay_canonical() {
    for tool_name in [
        "submit_web_ingest_run",
        "get_web_ingest_run",
        "list_web_ingest_run_pages",
        "cancel_web_ingest_run",
    ] {
        assert!(
            MCP_DIAGNOSTICS_TOOL_NAMES.contains(&tool_name),
            "missing MCP tool `{tool_name}` from canonical tool list"
        );
    }

    let submit_request: McpSubmitWebIngestRunRequest = serde_json::from_value(json!({
        "library": "default/docs",
        "seedUrl": "https://example.com/docs",
        "mode": "recursive_crawl",
        "boundaryPolicy": "allow_external",
        "maxDepth": 4,
        "maxPages": 80,
        "idempotencyKey": "crawl-1"
    }))
    .expect("submit request should deserialize");
    assert_eq!(submit_request.library, "default/docs");
    assert_eq!(submit_request.seed_url, "https://example.com/docs");
    assert_eq!(submit_request.mode, "recursive_crawl");
    assert_eq!(submit_request.boundary_policy.as_deref(), Some("allow_external"));
    assert_eq!(submit_request.max_depth, Some(4));
    assert_eq!(submit_request.max_pages, Some(80));
    assert_eq!(submit_request.idempotency_key.as_deref(), Some("crawl-1"));

    let run_id = Uuid::now_v7();
    let get_request: McpGetWebIngestRunRequest =
        serde_json::from_value(json!({ "runId": run_id })).expect("get request should deserialize");
    let list_pages_request: McpListWebIngestRunPagesRequest =
        serde_json::from_value(json!({ "runId": run_id }))
            .expect("list pages request should deserialize");
    let cancel_request: McpCancelWebIngestRunRequest =
        serde_json::from_value(json!({ "runId": run_id }))
            .expect("cancel request should deserialize");

    assert_eq!(get_request.run_id, run_id);
    assert_eq!(list_pages_request.run_id, run_id);
    assert_eq!(cancel_request.run_id, run_id);

    assert!(
        serde_json::from_value::<McpSubmitWebIngestRunRequest>(json!({
            "libraryId": Uuid::nil(),
            "seedUrl": "https://example.com/docs",
            "mode": "recursive_crawl"
        }))
        .is_err(),
        "legacy snake_case MCP request fields must be rejected"
    );
}
