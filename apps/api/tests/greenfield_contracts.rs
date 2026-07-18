use std::{collections::HashSet, fs, path::PathBuf};

use anyhow::Context as _;
use yaml_rust2::{
    parser::{Event, MarkedEventReceiver, Parser},
    scanner::Marker,
};

const CANONICAL_TAGS: &[&str] = &[
    "system",
    "catalog",
    "iam",
    "ai",
    "knowledge",
    "content",
    "ingest",
    "query",
    "runtime",
    "billing",
    "ops",
    "audit",
    "automation",
    "admin",
];

const CANONICAL_PATH_PREFIXES: &[&str] = &[
    "/v1/catalog",
    "/v1/iam",
    "/v1/ai",
    "/v1/knowledge",
    "/v1/content",
    "/v1/ingest",
    "/v1/query",
    "/v1/runtime",
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
pub(crate) fn load_openapi_contract_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts").join("openapi.gen.yaml");
    fs::read_to_string(&path).unwrap_or_default()
}

fn contains_legacy_vocabulary(contract: &str, legacy: &str) -> bool {
    let normalized = contract.to_ascii_lowercase();
    let legacy_is_prefix = legacy.ends_with('_');
    normalized.match_indices(legacy).any(|(start, _)| {
        let has_start_boundary = contract[..start]
            .chars()
            .next_back()
            .is_none_or(|previous| !previous.is_ascii_alphanumeric());
        let end = start.saturating_add(legacy.len());
        let has_end_boundary = legacy_is_prefix
            || contract[end..]
                .chars()
                .next()
                .is_none_or(|next| !next.is_ascii_alphanumeric() || next.is_uppercase());
        has_start_boundary && has_end_boundary
    })
}

/// Collects every YAML scalar that carries no whitespace. Structural
/// identifiers — path templates, schema names, property names, tag values,
/// enum members — are single tokens, while human prose (descriptions,
/// summaries) always contains spaces. Scanning only these scalars keeps the
/// vocabulary gate about the API surface instead of failing on ordinary
/// English ("the document collection") or MCP tool names quoted in
/// documentation (`get_runtime_execution`).
#[derive(Debug, Default)]
struct IdentifierScalarCollector {
    identifiers: Vec<String>,
}

impl MarkedEventReceiver for IdentifierScalarCollector {
    fn on_event(&mut self, event: Event, _mark: Marker) {
        if let Event::Scalar(value, ..) = event
            && !value.contains(char::is_whitespace)
            && !value.is_empty()
        {
            self.identifiers.push(value);
        }
    }
}

#[allow(
    clippy::expect_used,
    reason = "the detection helper must fail at the YAML parser boundary with its invariant message"
)]
pub(crate) fn detect_legacy_vocabulary_occurrences(contract: &str) -> Vec<String> {
    let mut parser = Parser::new_from_str(contract);
    let mut collector = IdentifierScalarCollector::default();
    parser.load(&mut collector, true).expect("OpenAPI contract should parse as valid YAML");

    FORBIDDEN_LEGACY_VOCABULARY
        .iter()
        .filter(|legacy| {
            collector
                .identifiers
                .iter()
                .any(|identifier| contains_legacy_vocabulary(identifier, legacy))
        })
        .map(std::string::ToString::to_string)
        .collect()
}

pub(crate) fn validate_greenfield_openapi_scaffold(contract: &str) -> Result<(), Vec<String>> {
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

pub(crate) fn assert_greenfield_openapi_scaffold(contract: &str) {
    let validation = validate_greenfield_openapi_scaffold(contract);
    assert!(
        validation.is_ok(),
        "greenfield OpenAPI scaffold validation failed: {:?}",
        validation.err().unwrap_or_default()
    );
}

pub(crate) fn assert_no_legacy_vocabulary(contract: &str) {
    let occurrences = detect_legacy_vocabulary_occurrences(contract);
    assert!(
        occurrences.is_empty(),
        "forbidden legacy vocabulary found in OpenAPI contract identifiers: {occurrences:?}"
    );
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

#[allow(
    clippy::expect_used,
    reason = "the assertion helper must fail at the YAML parser boundary with its invariant message"
)]
pub(crate) fn assert_no_duplicate_yaml_mapping_keys(contract: &str) {
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
  - name: system
  - name: catalog
  - name: iam
  - name: ai
  - name: knowledge
  - name: content
  - name: ingest
  - name: query
  - name: runtime
  - name: billing
  - name: ops
  - name: audit
  - name: automation
  - name: admin
x-greenfield-scaffold:
  canonicalPathPrefixes:
    - /v1/catalog
    - /v1/iam
    - /v1/ai
    - /v1/knowledge
    - /v1/content
    - /v1/ingest
    - /v1/query
    - /v1/runtime
    - /v1/billing
    - /v1/ops
    - /v1/audit
    - /v1/mcp
paths:
  /v1/catalog/workspaces: {}
  /v1/iam/me: {}
  /v1/ai/providers: {}
  /v1/knowledge/libraries/{libraryId}/entities: {}
  /v1/content/documents/{documentId}: {}
  /v1/ingest/jobs/{jobId}: {}
  /v1/query/sessions: {}
  /v1/runtime/executions/{runtimeExecutionId}: {}
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
fn legacy_helpers_detect_camel_case_vocabulary() {
    let sample = r"
components:
  schemas:
    ProjectId:
      ownerKind: runtime_execution
";

    assert_eq!(
        detect_legacy_vocabulary_occurrences(sample),
        vec!["project".to_string(), "runtime_".to_string()]
    );
}

#[test]
fn legacy_helpers_ignore_prose_and_quoted_tool_names_in_descriptions() {
    let sample = r"
paths:
  /v1/knowledge/libraries/{libraryId}/entities:
    get:
      description: >-
        Library that owns the document collection. The MCP tool
        `get_runtime_execution` is a thin wrapper over this same read.
";

    assert!(detect_legacy_vocabulary_occurrences(sample).is_empty());
}

#[test]
fn legacy_helpers_do_not_match_vocabulary_inside_canonical_words() {
    let sample = r"
openapi: 3.1.0
components:
  schemas:
    projection:
      description: Canonical evidence projection.
";

    assert!(detect_legacy_vocabulary_occurrences(sample).is_empty());
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

// `actual_contract_contains_greenfield_scaffold_markers` and
// `actual_fresh_deploy_contract_surface_uses_workspace_and_library_only` were
// removed in sub-sprint 1d. They guarded the hand-maintained
// `apps/api/contracts/ironrag.openapi.yaml` for `x-greenfield-scaffold` markers
// and a fresh-bootstrap discovery block. The yaml is now generated from
// `#[utoipa::path]` annotations on Rust handlers, so the guards belong on the
// Rust source. The legacy vocabulary check above still applies to the emitted
// document and continues to gate vocabulary regressions.

#[test]
fn actual_contract_exposes_canonical_session_and_admin_support_routes() {
    let contract = load_openapi_contract_text();

    assert!(contract.contains("/v1/iam/session/login"));
    assert!(contract.contains("/v1/iam/session/logout"));
    assert!(contract.contains("/v1/iam/users/{userId}/access"));
    assert!(contract.contains("/v1/ai/accounts"));
    assert!(contract.contains("/v1/ai/bindings"));
    assert!(contract.contains("/v1/query/sessions"));
    // The grants collection was deliberately collapsed into the single
    // declarative access document in the v2 redesign.
    assert!(!contract.contains("/v1/iam/grants"));
}

#[test]
fn actual_contract_assistant_prompt_documents_transport_agnostic_mcp_clients() -> anyhow::Result<()>
{
    let contract = load_openapi_contract_text();
    let prompt_schema = contract
        .split("AssistantSystemPromptResponse:")
        .nth(1)
        .and_then(|section| section.split("AssistantTechnicalFactReference:").next())
        .context("assistant system prompt schema is absent")?;

    for required in [
        "transport-agnostic",
        "Claude Desktop",
        "Claude Code",
        "Cursor",
        "Codex",
        "VS Code",
        "Continue/Cline/Roo",
        "Zed",
        "Hermes",
    ] {
        assert!(prompt_schema.contains(required), "missing `{required}` in prompt schema");
    }
    Ok(())
}

#[test]
fn actual_contract_exposes_canonical_content_and_processing_routes() {
    let contract = load_openapi_contract_text();

    assert!(contract.contains("/v1/content/libraries/{libraryId}/documents"));
    assert!(contract.contains("/v1/content/documents/{documentId}"));
    assert!(contract.contains("/v1/content/documents/{documentId}/revisions"));
    assert!(contract.contains("/v1/ops/operations/{operationId}"));
    assert!(contract.contains("/v1/ingest/jobs/{jobId}"));
    assert!(contract.contains("/v1/ingest/attempts/{attemptId}"));
    assert!(contract.contains("/v1/knowledge/libraries/{libraryId}/summary"));
    assert!(contract.contains("/v1/knowledge/libraries/{libraryId}/search"));
    assert!(!contract.contains("/v1/knowledge/libraries/{libraryId}/readiness"));
    assert!(!contract.contains("/v1/knowledge/libraries/{libraryId}/graph/coverage"));
    // The v2 redesign removed the mutation sub-resource and the standalone
    // /v1/search surface: revisions + async operations replace mutations, and
    // search is a library-scoped knowledge route.
    assert!(!contract.contains("/v1/content/mutations"));
    assert!(!contract.contains("/v1/search/documents"));
}
