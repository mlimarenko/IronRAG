#[path = "greenfield_contracts.rs"]
mod greenfield_contracts;

use anyhow::Context as _;
use serde_json::json;
use serde_yaml::Value;
use uuid::Uuid;

use ironrag_backend::{
    interfaces::http::mcp::MCP_DIAGNOSTICS_TOOL_NAMES,
    mcp_types::{
        McpCancelWebIngestRunRequest, McpGetWebIngestRunRequest, McpListWebIngestRunPagesRequest,
        McpSubmitWebIngestRunRequest,
    },
    shared::web::ingest::{
        DEFAULT_WEB_CRAWL_DEPTH, DEFAULT_WEB_CRAWL_MAX_PAGES, WebBoundaryPolicy,
        WebClassificationReason, WebRunCounts, WebRunFailureCode, derive_terminal_run_state,
        validate_web_run_settings,
    },
};

fn openapi_contract() -> anyhow::Result<Value> {
    serde_yaml::from_str(&greenfield_contracts::load_openapi_contract_text())
        .context("OpenAPI contract is not valid YAML")
}

fn mapping_child<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a Value> {
    value
        .as_mapping()
        .and_then(|mapping| mapping.get(Value::String(key.to_string())))
        .with_context(|| format!("OpenAPI mapping key `{key}` is absent"))
}

fn component_schema<'a>(contract: &'a Value, name: &str) -> anyhow::Result<&'a Value> {
    let components = mapping_child(contract, "components")?;
    let schemas = mapping_child(components, "schemas")?;
    mapping_child(schemas, name)
}

fn property_schema<'a>(schema: &'a Value, name: &str) -> anyhow::Result<&'a Value> {
    let properties = mapping_child(schema, "properties")?;
    mapping_child(properties, name)
}

fn string_array(value: &Value) -> anyhow::Result<Vec<String>> {
    let values = value.as_sequence().context("OpenAPI value is not an array")?;
    values
        .iter()
        .map(|item| item.as_str().map(str::to_string).context("OpenAPI array item is not a string"))
        .collect()
}

fn string_array_child(value: &Value, key: &str) -> anyhow::Result<Vec<String>> {
    string_array(mapping_child(value, key)?)
}

fn string_array_from<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}

#[test]
fn web_ingest_rest_surface_keeps_canonical_routes_and_runtime_defaults() -> anyhow::Result<()> {
    let contract = greenfield_contracts::load_openapi_contract_text();
    let openapi = openapi_contract()?;

    for path in [
        "/v1/content/web-runs:",
        "/v1/content/web-runs/{runId}:",
        "/v1/content/web-runs/{runId}/pages:",
        "/v1/content/web-runs/{runId}/cancel:",
    ] {
        assert!(contract.contains(path), "missing web ingest REST path `{path}`");
    }

    let request_schema = component_schema(&openapi, "CreateWebIngestRunRequest")?;
    assert_eq!(
        string_array_child(request_schema, "required")?,
        ["libraryId", "seedUrl", "mode", "crawlFilter", "materializationFilter"],
        "CreateWebIngestRunRequest must keep canonical required fields",
    );
    assert_eq!(
        mapping_child(property_schema(request_schema, "mode")?, "$ref")?
            .as_str()
            .context("mode reference is not a string")?,
        "#/components/schemas/WebIngestMode",
        "CreateWebIngestRunRequest.mode must use the canonical mode enum",
    );
    assert_eq!(
        string_array_child(component_schema(&openapi, "WebIngestMode")?, "enum")?,
        ["single_page", "recursive_crawl"],
        "web ingest mode enum must stay canonical in OpenAPI",
    );
    assert_eq!(
        string_array_child(component_schema(&openapi, "WebBoundaryPolicy")?, "enum")?,
        [
            WebBoundaryPolicy::SameHost.as_str(),
            WebBoundaryPolicy::SameHostAndSubdomains.as_str(),
            WebBoundaryPolicy::AllowExternal.as_str(),
        ],
        "web ingest boundary enum must stay canonical in OpenAPI",
    );

    let single_page_defaults = validate_web_run_settings("single_page", None, Some(9), None)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(single_page_defaults.mode, "single_page");
    assert_eq!(single_page_defaults.boundary_policy, "same_host");
    assert_eq!(single_page_defaults.max_depth, 0);
    assert_eq!(single_page_defaults.max_pages, DEFAULT_WEB_CRAWL_MAX_PAGES);

    let recursive_defaults = validate_web_run_settings("recursive_crawl", None, None, None)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(recursive_defaults.mode, "recursive_crawl");
    assert_eq!(recursive_defaults.boundary_policy, "same_host");
    assert_eq!(recursive_defaults.max_depth, DEFAULT_WEB_CRAWL_DEPTH);
    assert_eq!(recursive_defaults.max_pages, DEFAULT_WEB_CRAWL_MAX_PAGES);
    Ok(())
}

#[test]
fn web_ingest_contract_enums_cover_runtime_vocabulary_and_partial_count_grammar()
-> anyhow::Result<()> {
    let openapi = openapi_contract()?;

    assert_eq!(
        string_array_child(component_schema(&openapi, "WebIngestRunState")?, "enum")?,
        [
            "accepted",
            "discovering",
            "processing",
            "completed",
            "completed_partial",
            "failed",
            "canceled"
        ],
        "run state enum must keep completed_partial in OpenAPI",
    );
    assert_eq!(
        string_array_child(component_schema(&openapi, "WebRunCounts")?, "required")?,
        [
            "discovered",
            "eligible",
            "processed",
            "queued",
            "processing",
            "duplicates",
            "excluded",
            "blocked",
            "failed",
            "canceled"
        ],
        "WebRunCounts must keep queued and processing grammar",
    );

    assert_eq!(
        string_array_child(component_schema(&openapi, "WebClassificationReason")?, "enum")?,
        string_array_from(WebClassificationReason::ALL.map(WebClassificationReason::as_str)),
        "classification reason enum must cover the runtime vocabulary",
    );
    assert_eq!(
        string_array_child(component_schema(&openapi, "WebRunFailureCode")?, "enum")?,
        string_array_from(WebRunFailureCode::ALL.map(WebRunFailureCode::as_str)),
        "failure code enum must cover the runtime vocabulary",
    );

    let completed_partial = derive_terminal_run_state(&WebRunCounts {
        processed: 2,
        failed: 1,
        ..WebRunCounts::default()
    });
    assert_eq!(completed_partial.as_str(), "completed_partial");
    Ok(())
}

#[test]
fn web_ingest_mcp_tool_vocabulary_and_request_fields_stay_canonical() -> anyhow::Result<()> {
    for tool_name in ["submit_web_run", "get_web_run", "list_web_run_pages", "cancel_web_run"] {
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
        "crawlFilter": {
            "allowPatterns": [
                {"kind": "path_prefix", "value": "/docs"}
            ],
            "blockPatterns": [
                {"kind": "path_prefix", "value": "/docs/archive"}
            ]
        },
        "materializationFilter": {
            "allowPatterns": [],
            "blockPatterns": []
        },
        "idempotencyKey": "crawl-1"
    }))?;
    assert_eq!(submit_request.library, "default/docs");
    assert_eq!(submit_request.seed_url, "https://example.com/docs");
    assert_eq!(submit_request.mode, "recursive_crawl");
    assert_eq!(submit_request.boundary_policy.as_deref(), Some("allow_external"));
    assert_eq!(submit_request.max_depth, Some(4));
    assert_eq!(submit_request.max_pages, Some(80));
    assert_eq!(submit_request.crawl_filter.allow_patterns.len(), 1);
    assert_eq!(submit_request.crawl_filter.block_patterns.len(), 1);
    assert_eq!(submit_request.idempotency_key.as_deref(), Some("crawl-1"));

    let run_id = Uuid::now_v7();
    let get_request: McpGetWebIngestRunRequest =
        serde_json::from_value(json!({ "runId": run_id }))?;
    let list_pages_request: McpListWebIngestRunPagesRequest =
        serde_json::from_value(json!({ "runId": run_id }))?;
    let cancel_request: McpCancelWebIngestRunRequest =
        serde_json::from_value(json!({ "runId": run_id }))?;

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

    let single_stage_filter_key = ["url", "Filter"].concat();
    let mut single_stage_filter_request = json!({
        "library": "default/docs",
        "seedUrl": "https://example.com/docs",
        "mode": "recursive_crawl",
        "crawlFilter": {
            "allowPatterns": [],
            "blockPatterns": []
        },
        "materializationFilter": {
            "allowPatterns": [],
            "blockPatterns": []
        }
    });
    single_stage_filter_request
        .as_object_mut()
        .context("synthetic request is not an object")?
        .insert(
            single_stage_filter_key,
            json!({
                "mode": "blocklist",
                "patterns": []
            }),
        );
    assert!(
        serde_json::from_value::<McpSubmitWebIngestRunRequest>(single_stage_filter_request)
            .is_err(),
        "single-stage filter aliases must be rejected"
    );

    assert!(
        serde_json::from_value::<McpSubmitWebIngestRunRequest>(json!({
            "library": "default/docs",
            "seedUrl": "https://example.com/docs",
            "mode": "recursive_crawl",
            "crawlFilter": {},
            "materializationFilter": {
                "allowPatterns": [],
                "blockPatterns": []
            }
        }))
        .is_err(),
        "filter objects must require both allowPatterns and blockPatterns"
    );
    Ok(())
}
