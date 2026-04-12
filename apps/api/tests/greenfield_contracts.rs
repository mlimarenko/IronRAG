#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::{collections::HashSet, fs, path::PathBuf};

use yaml_rust2::{
    parser::{Event, MarkedEventReceiver, Parser},
    scanner::Marker,
};

const CANONICAL_TAGS: &[&str] = &[
    "catalog",
    "iam",
    "ai",
    "knowledge",
    "content",
    "ingest",
    "search",
    "query",
    "billing",
    "ops",
    "audit",
    "automation",
];

const CANONICAL_PATH_PREFIXES: &[&str] = &[
    "/v1/catalog",
    "/v1/iam",
    "/v1/ai",
    "/v1/knowledge",
    "/v1/content",
    "/v1/ingest",
    "/v1/search",
    "/v1/query",
    "/v1/billing",
    "/v1/ops",
    "/v1/audit",
    "/v1/mcp",
];

const FORBIDDEN_LEGACY_VOCABULARY: &[&str] = &[
    "project",
    "projects",
    "collection",
    "collections",
    "runtime_",
    "ui_",
    "mcp_memory",
    "provider_account",
    "model_profile",
];

#[must_use]
pub fn load_openapi_contract_text() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts").join("ironrag.openapi.yaml");
    fs::read_to_string(&path).unwrap_or_default()
}

#[must_use]
pub const fn canonical_tags() -> &'static [&'static str] {
    CANONICAL_TAGS
}

#[must_use]
pub const fn canonical_path_prefixes() -> &'static [&'static str] {
    CANONICAL_PATH_PREFIXES
}

#[must_use]
pub const fn forbidden_legacy_vocabulary() -> &'static [&'static str] {
    FORBIDDEN_LEGACY_VOCABULARY
}

pub fn detect_legacy_vocabulary_occurrences(contract: &str) -> Vec<String> {
    let normalized = contract.to_ascii_lowercase();
    FORBIDDEN_LEGACY_VOCABULARY
        .iter()
        .filter(|legacy| normalized.contains(**legacy))
        .map(std::string::ToString::to_string)
        .collect()
}

pub fn validate_greenfield_openapi_scaffold(contract: &str) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    for tag in CANONICAL_TAGS {
        let needle = format!("- name: {tag}");
        if !contract.contains(&needle) {
            errors.push(format!("missing canonical tag `{tag}`"));
        }
    }

    for prefix in CANONICAL_PATH_PREFIXES {
        if !contract.contains(prefix) {
            errors.push(format!("missing canonical path prefix `{prefix}`"));
        }
    }

    if !contract.contains("x-greenfield-scaffold:") {
        errors.push("missing x-greenfield-scaffold metadata block".to_string());
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

pub fn assert_greenfield_openapi_scaffold(contract: &str) {
    let validation = validate_greenfield_openapi_scaffold(contract);
    assert!(
        validation.is_ok(),
        "greenfield OpenAPI scaffold validation failed: {:?}",
        validation.err().unwrap_or_default()
    );
}

pub fn assert_no_legacy_vocabulary(contract: &str) {
    let normalized = contract.to_ascii_lowercase();
    for legacy in FORBIDDEN_LEGACY_VOCABULARY {
        assert!(
            !normalized.contains(legacy),
            "forbidden legacy vocabulary `{legacy}` found in OpenAPI contract"
        );
    }
}

pub fn assert_contains_canonical_tags(contract: &str) {
    for tag in CANONICAL_TAGS {
        assert!(contract.contains(&format!("- name: {tag}")), "missing canonical tag `{tag}`");
    }
}

pub fn assert_contains_canonical_paths(contract: &str) {
    for prefix in CANONICAL_PATH_PREFIXES {
        assert!(contract.contains(prefix), "missing canonical path prefix `{prefix}`");
    }
}

pub fn assert_fresh_deploy_surface_uses_canonical_vocabulary(contract: &str) {
    let fresh_bootstrap_section = contract
        .split("/v1/iam/bootstrap/setup:")
        .nth(1)
        .and_then(|section| section.split("/v1/mcp:").next())
        .expect("fresh bootstrap path block present in contract");
    let discovery_section = contract
        .split("/v1/openapi/ironrag.openapi.yaml:")
        .nth(1)
        .and_then(|section| section.split("/v1/iam/bootstrap/setup:").next())
        .expect("openapi discovery path block present in contract");
    let scaffold_section = contract
        .split("freshBootstrapDiscovery:")
        .nth(1)
        .and_then(|section| section.split("scaffoldStatus:").next())
        .expect("fresh bootstrap discovery block present in contract");

    for section in [fresh_bootstrap_section, discovery_section, scaffold_section] {
        let normalized = section.to_ascii_lowercase();
        assert!(
            !normalized.contains("project"),
            "fresh-deploy contract section leaked legacy `project` vocabulary"
        );
        assert!(
            !normalized.contains("collection"),
            "fresh-deploy contract section leaked legacy `collection` vocabulary"
        );
    }

    assert!(contract.contains("CatalogWorkspace"));
    assert!(contract.contains("CatalogLibrary"));
    assert!(contract.contains("ArangoDB"));
    assert!(contract.contains("/v1/iam/bootstrap/setup"));
    assert!(contract.contains("/v1/openapi/ironrag.openapi.yaml"));
}

#[derive(Debug)]
enum YamlContainer {
    Mapping { keys: HashSet<String>, expecting_key: bool },
    Sequence,
}

#[derive(Debug, Default)]
struct DuplicateYamlKeyCollector {
    containers: Vec<YamlContainer>,
    duplicates: Vec<String>,
}

impl DuplicateYamlKeyCollector {
    fn on_scalar(&mut self, key: String, mark: Marker) {
        let Some(YamlContainer::Mapping { keys, expecting_key }) = self.containers.last_mut()
        else {
            return;
        };

        if *expecting_key {
            if !keys.insert(key.clone()) {
                self.duplicates.push(format!(
                    "duplicate YAML key `{key}` at line {}, column {}",
                    mark.line() + 1,
                    mark.col() + 1
                ));
            }
            *expecting_key = false;
        } else {
            *expecting_key = true;
        }
    }

    fn finish_nested_value(&mut self) {
        if let Some(YamlContainer::Mapping { expecting_key, .. }) = self.containers.last_mut()
            && !*expecting_key
        {
            *expecting_key = true;
        }
    }
}

impl MarkedEventReceiver for DuplicateYamlKeyCollector {
    fn on_event(&mut self, event: Event, mark: Marker) {
        match event {
            Event::MappingStart(..) => self
                .containers
                .push(YamlContainer::Mapping { keys: HashSet::new(), expecting_key: true }),
            Event::MappingEnd => {
                let _ = self.containers.pop();
                self.finish_nested_value();
            }
            Event::SequenceStart(..) => self.containers.push(YamlContainer::Sequence),
            Event::SequenceEnd => {
                let _ = self.containers.pop();
                self.finish_nested_value();
            }
            Event::Scalar(value, ..) => self.on_scalar(value, mark),
            Event::Alias(anchor) => self.on_scalar(format!("*{anchor}"), mark),
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart
            | Event::DocumentEnd => {}
        }
    }
}

pub fn assert_no_duplicate_yaml_mapping_keys(contract: &str) {
    let mut parser = Parser::new_from_str(contract);
    let mut collector = DuplicateYamlKeyCollector::default();
    parser.load(&mut collector, true).expect("OpenAPI contract should parse as valid YAML");
    assert!(
        collector.duplicates.is_empty(),
        "OpenAPI contract contains duplicate YAML keys: {:?}",
        collector.duplicates
    );
}

#[test]
fn scaffold_helpers_accept_greenfield_shaped_contract() {
    let sample = r"
openapi: 3.1.0
tags:
  - name: catalog
  - name: iam
  - name: ai
  - name: knowledge
  - name: content
  - name: ingest
  - name: search
  - name: query
  - name: billing
  - name: ops
  - name: audit
  - name: automation
x-greenfield-scaffold:
  canonicalPathPrefixes:
    - /v1/catalog
    - /v1/iam
    - /v1/ai
    - /v1/knowledge
    - /v1/content
    - /v1/ingest
    - /v1/search
    - /v1/query
    - /v1/billing
    - /v1/ops
    - /v1/audit
    - /v1/mcp
paths:
  /v1/catalog/workspaces: {}
  /v1/iam/me: {}
  /v1/ai/providers: {}
  /v1/knowledge/libraries/{libraryId}/entities: {}
  /v1/content/documents: {}
  /v1/ingest/jobs/{jobId}: {}
  /v1/search/documents: {}
  /v1/query/sessions: {}
  /v1/billing/provider-calls: {}
  /v1/ops/operations/{operationId}: {}
  /v1/audit/events: {}
  /v1/mcp: {}
";

    assert_greenfield_openapi_scaffold(sample);
}

#[test]
fn contract_has_no_duplicate_yaml_mapping_keys() {
    let contract = load_openapi_contract_text();
    assert_no_duplicate_yaml_mapping_keys(&contract);
}

#[test]
fn legacy_helpers_detect_forbidden_vocabulary() {
    let sample = r"
openapi: 3.1.0
tags:
  - name: catalog
paths:
  /v1/catalog/workspaces: {}
  /v1/projects: {}
  /v1/runtime_documents: {}
";

    let result = detect_legacy_vocabulary_occurrences(sample);
    assert!(result.iter().any(|token| token == "projects"));
    assert!(result.iter().any(|token| token == "runtime_"));
    assert_no_legacy_vocabulary("openapi: 3.1.0\npaths:\n  /v1/catalog/workspaces: {}\n");
}

#[test]
fn actual_contract_contains_greenfield_scaffold_markers() {
    let contract = load_openapi_contract_text();
    let result = validate_greenfield_openapi_scaffold(&contract);

    assert!(
        result.is_ok(),
        "expected actual contract to contain greenfield scaffold markers: {:?}",
        result.err().unwrap_or_default()
    );
}

#[test]
fn actual_contract_no_longer_reports_legacy_vocabulary_debt() {
    let contract = load_openapi_contract_text();
    let result = detect_legacy_vocabulary_occurrences(&contract);

    assert!(
        result.is_empty(),
        "expected actual contract to be free of legacy vocabulary debt, found: {result:?}"
    );
}

#[test]
fn actual_fresh_deploy_contract_surface_uses_workspace_and_library_only() {
    let contract = load_openapi_contract_text();
    assert_fresh_deploy_surface_uses_canonical_vocabulary(&contract);
}

#[test]
fn actual_contract_exposes_canonical_session_and_admin_support_routes() {
    let contract = load_openapi_contract_text();

    assert!(contract.contains("/v1/iam/session/login"));
    assert!(contract.contains("/v1/iam/session/logout"));
    assert!(contract.contains("/v1/iam/grants"));
    assert!(contract.contains("/v1/ai/model-presets"));
    assert!(contract.contains("/v1/query/sessions"));
}

#[test]
fn actual_contract_exposes_canonical_content_and_processing_routes() {
    let contract = load_openapi_contract_text();

    assert!(contract.contains("/v1/content/documents"));
    assert!(contract.contains("/v1/content/mutations"));
    assert!(contract.contains("/v1/ingest/jobs/{jobId}"));
    assert!(contract.contains("/v1/ingest/attempts/{attemptId}"));
    assert!(contract.contains("/v1/knowledge/libraries/{libraryId}/summary"));
    assert!(contract.contains("/v1/knowledge/libraries/{libraryId}/search/documents"));
    assert!(!contract.contains("/v1/knowledge/libraries/{libraryId}/readiness"));
    assert!(!contract.contains("/v1/knowledge/libraries/{libraryId}/graph/coverage"));
    assert!(contract.contains("/v1/search/documents"));
}
