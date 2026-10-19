use super::{ExtractTechnicalFactsCommand, TechnicalFactService};
use crate::shared::extraction::{
    structured_document::{StructuredBlockData, StructuredBlockKind},
    table_markdown::parse_markdown_table_row,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// extracts_branded_identifiers_from_catalog_link_list_items — REMOVED.
// Branded identifier heuristic was the noisiest extractor and is now
// delegated to the LLM's entity extraction pipeline.

#[test]
fn branded_identifier_extraction_no_longer_produces_facts() {
    // Verify the branded heuristic is truly gone — no `Identifier`
    // facts from heading phrases or list item links.
    let service = TechnicalFactService::new();
    let result = service
        .extract_from_blocks(
            ExtractTechnicalFactsCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                blocks: vec![
                    StructuredBlockData {
                        block_id: Uuid::now_v7(),
                        ordinal: 0,
                        block_kind: StructuredBlockKind::Heading,
                        text: "Acme Software Products - Acme Software Products - Example"
                            .to_string(),
                        normalized_text:
                            "Acme Software Products - Acme Software Products - Example".to_string(),
                        heading_trail: vec![
                            "Acme Software Products - Acme Software Products - Example".to_string(),
                        ],
                        section_path: vec!["acme-software-products".to_string()],
                        page_number: None,
                        source_span: None,
                        parent_block_id: None,
                        table_coordinates: None,
                        code_language: None,
                        is_boilerplate: false,
                    },
                    StructuredBlockData {
                        block_id: Uuid::now_v7(),
                        ordinal: 1,
                        block_kind: StructuredBlockKind::ListItem,
                        text:
                            "- [Operations Console](https://docs.example.test/operations-console)"
                                .to_string(),
                        normalized_text:
                            "- [Operations Console](https://docs.example.test/operations-console)"
                                .to_string(),
                        heading_trail: vec![
                            "Acme Software Products - Acme Software Products - Example".to_string(),
                        ],
                        section_path: vec!["acme-software-products".to_string()],
                        page_number: None,
                        source_span: None,
                        parent_block_id: None,
                        table_coordinates: None,
                        code_language: None,
                        is_boilerplate: false,
                    },
                    StructuredBlockData {
                        block_id: Uuid::now_v7(),
                        ordinal: 2,
                        block_kind: StructuredBlockKind::ListItem,
                        text: "- [POS](https://docs.example.test/pos)".to_string(),
                        normalized_text: "- [POS](https://docs.example.test/pos)".to_string(),
                        heading_trail: vec![
                            "Acme Software Products - Acme Software Products - Example".to_string(),
                        ],
                        section_path: vec!["acme-software-products".to_string()],
                        page_number: None,
                        source_span: None,
                        parent_block_id: None,
                        table_coordinates: None,
                        code_language: None,
                        is_boilerplate: false,
                    },
                ],
            },
            &CancellationToken::new(),
        )
        .expect("technical facts extracted");

    let identifiers = result
        .facts
        .iter()
        .filter(|fact| fact.fact_kind.as_str() == "identifier")
        .map(|fact| fact.display_value.as_str())
        .collect::<Vec<_>>();

    assert!(
        identifiers.is_empty(),
        "branded identifier heuristic should no longer produce facts, got: {:?}",
        identifiers
    );
}

#[test]
fn extracts_endpoint_methods_and_query_parameter_names() {
    let service = TechnicalFactService::new();
    let result = service
        .extract_from_blocks(
            ExtractTechnicalFactsCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                blocks: vec![StructuredBlockData {
                    block_id: Uuid::now_v7(),
                    ordinal: 0,
                    block_kind: StructuredBlockKind::EndpointBlock,
                    text: "GET https://api.example.test/orders?pageNumber=1&pageSize=25"
                        .to_string(),
                    normalized_text: "GET https://api.example.test/orders?pageNumber=1&pageSize=25"
                        .to_string(),
                    heading_trail: vec!["Orders".to_string()],
                    section_path: vec!["orders".to_string()],
                    page_number: None,
                    source_span: None,
                    parent_block_id: None,
                    table_coordinates: None,
                    code_language: None,
                    is_boilerplate: false,
                }],
            },
            &CancellationToken::new(),
        )
        .expect("technical facts extracted");

    let fact_kinds = result
        .facts
        .iter()
        .map(|fact| (fact.fact_kind.as_str().to_string(), fact.display_value.clone()))
        .collect::<Vec<_>>();

    assert!(fact_kinds.contains(&("http_method".to_string(), "GET".to_string())));
    assert!(fact_kinds.contains(&(
        "url".to_string(),
        "https://api.example.test/orders?pageNumber=1&pageSize=25".to_string()
    )));
    assert!(fact_kinds.contains(&("endpoint_path".to_string(), "/orders".to_string())));
    assert!(fact_kinds.contains(&("parameter_name".to_string(), "pageNumber".to_string())));
    assert!(fact_kinds.contains(&("parameter_name".to_string(), "pageSize".to_string())));
}

#[test]
fn table_rows_skip_business_table_url_noise_for_fact_extraction() {
    let service = TechnicalFactService::new();
    let raw_row =
        "| 1 | FAB0d41d5b5d22c | Ferrell LLC | https://price.net/ | Papua New Guinea | Plastics |";
    assert_eq!(parse_markdown_table_row(raw_row).len(), 6);
    let result = service.extract_from_blocks(ExtractTechnicalFactsCommand {
        revision_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        blocks: vec![
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::Table,
                text: "| Index | Organization Id | Name | Website | Country | Industry |\n| --- | --- | --- | --- | --- | --- |\n| 1 | FAB0d41d5b5d22c | Ferrell LLC | https://price.net/ | Papua New Guinea | Plastics |".to_string(),
                normalized_text: "| Index | Organization Id | Name | Website | Country | Industry |\n| --- | --- | --- | --- | --- | --- |\n| 1 | FAB0d41d5b5d22c | Ferrell LLC | https://price.net/ | Papua New Guinea | Plastics |".to_string(),
                heading_trail: vec!["organizations-100".to_string()],
                section_path: vec!["organizations_100".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 1,
                block_kind: StructuredBlockKind::TableRow,
                text: raw_row.to_string(),
                normalized_text: "Sheet: organizations-100 | Row 1 | Index: 1 | Organization Id: FAB0d41d5b5d22c | Name: Ferrell LLC | Website: https://price.net/ | Country: Papua New Guinea | Industry: Plastics".to_string(),
                heading_trail: vec!["organizations-100".to_string()],
                section_path: vec!["organizations_100".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
        ],
    }, &CancellationToken::new())
    .expect("technical facts extracted");

    assert!(!result.facts.iter().any(|fact| {
        fact.fact_kind.as_str() == "parameter_name" && fact.display_value == "Sheet"
    }));
    assert!(!result.facts.iter().any(|fact| {
        fact.fact_kind.as_str() == "parameter_name" && fact.display_value == "Index"
    }));
    assert!(result.facts.is_empty());
}

#[test]
fn table_rows_keep_formal_endpoint_facts_without_semantic_header_names() {
    let service = TechnicalFactService::new();
    let result = service.extract_from_blocks(ExtractTechnicalFactsCommand {
        revision_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        blocks: vec![StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 0,
            block_kind: StructuredBlockKind::TableRow,
            text: "| GET | /orders | https://api.example.test/orders | 200 |".to_string(),
            normalized_text: "Sheet: API | Row 1 | A: GET | B: /orders | C: https://api.example.test/orders | D: 200".to_string(),
            heading_trail: vec!["api".to_string()],
            section_path: vec!["api".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        }],
    }, &CancellationToken::new())
    .expect("technical facts extracted");

    let fact_kinds = result
        .facts
        .iter()
        .map(|fact| (fact.fact_kind.as_str().to_string(), fact.display_value.clone()))
        .collect::<Vec<_>>();
    assert!(fact_kinds.contains(&("http_method".to_string(), "GET".to_string())));
    assert!(fact_kinds.contains(&("endpoint_path".to_string(), "/orders".to_string())));
    assert!(
        fact_kinds.contains(&("url".to_string(), "https://api.example.test/orders".to_string()))
    );
}

#[test]
fn table_rows_abstain_when_only_semantic_headers_imply_technical_meaning() {
    let facts = extract_facts(vec![StructuredBlockData {
        block_id: Uuid::now_v7(),
        ordinal: 0,
        block_kind: StructuredBlockKind::TableRow,
        text: "| 404 | 8080 |".to_string(),
        normalized_text: "Sheet: Sample | Row 1 | Status: 404 | Port: 8080".to_string(),
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        page_number: None,
        source_span: None,
        parent_block_id: None,
        table_coordinates: None,
        code_language: None,
        is_boilerplate: false,
    }]);

    assert!(facts.is_empty(), "ambiguous table values must not become technical facts");
}

fn make_test_block(
    kind: StructuredBlockKind,
    text: &str,
    code_language: Option<&str>,
) -> StructuredBlockData {
    StructuredBlockData {
        block_id: Uuid::now_v7(),
        ordinal: 0,
        block_kind: kind,
        text: text.to_string(),
        normalized_text: text.to_string(),
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        page_number: None,
        source_span: None,
        parent_block_id: None,
        table_coordinates: None,
        code_language: code_language.map(str::to_string),
        is_boilerplate: false,
    }
}

fn extract_facts(
    blocks: Vec<StructuredBlockData>,
) -> Vec<crate::domains::knowledge::TypedTechnicalFact> {
    TechnicalFactService::new()
        .extract_from_blocks(
            ExtractTechnicalFactsCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                blocks,
            },
            &CancellationToken::new(),
        )
        .expect("technical facts extracted")
        .facts
}

#[test]
fn extracts_environment_variables() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::Paragraph,
        "Set $DATABASE_URL and process.env.API_KEY before starting",
        None,
    )]);

    let env_vars: Vec<_> =
        facts.iter().filter(|f| f.fact_kind.as_str() == "environment_variable").collect();

    assert!(
        env_vars.len() >= 2,
        "expected at least 2 environment variables, found {}: {:?}",
        env_vars.len(),
        env_vars.iter().map(|f| &f.display_value).collect::<Vec<_>>()
    );
}

#[test]
fn environment_context_does_not_promote_bare_uppercase_tokens() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::Paragraph,
        "environment DATABASE_URL",
        None,
    )]);

    assert!(!facts.iter().any(|fact| fact.fact_kind.as_str() == "environment_variable"));
}

#[test]
fn extracts_version_numbers() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::Paragraph,
        "Requires version v2.3.1 or later",
        None,
    )]);

    let versions: Vec<_> =
        facts.iter().filter(|f| f.fact_kind.as_str() == "version_number").collect();

    assert!(!versions.is_empty(), "expected at least one VersionNumber fact");
    assert!(
        versions.iter().any(|f| f.display_value.contains("2.3.1")),
        "expected a version containing '2.3.1', got: {:?}",
        versions.iter().map(|f| &f.display_value).collect::<Vec<_>>()
    );
}

#[test]
fn version_context_does_not_promote_bare_numeric_literals() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::Paragraph,
        "version 2.3.1; release 2026.07",
        None,
    )]);

    assert!(!facts.iter().any(|fact| fact.fact_kind.as_str() == "version_number"));
}

#[test]
fn error_context_does_not_promote_an_ambiguous_identifier() {
    let facts =
        extract_facts(vec![make_test_block(StructuredBlockKind::Paragraph, "error E1234", None)]);

    assert!(!facts.iter().any(|fact| fact.fact_kind.as_str() == "error_code"));
}

#[test]
fn semantic_port_context_does_not_promote_a_bare_number() {
    let facts =
        extract_facts(vec![make_test_block(StructuredBlockKind::Paragraph, "port 8080", None)]);

    assert!(!facts.iter().any(|fact| fact.fact_kind.as_str() == "port"));
}

#[test]
fn explicit_url_authority_keeps_its_port() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::Paragraph,
        "https://api.example.test:8443/orders",
        None,
    )]);

    assert!(
        facts
            .iter()
            .any(|fact| { fact.fact_kind.as_str() == "port" && fact.display_value == "8443" })
    );
}

#[test]
fn semantic_status_context_does_not_promote_a_bare_number() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::EndpointBlock,
        "status 404",
        None,
    )]);

    assert!(!facts.iter().any(|fact| fact.fact_kind.as_str() == "status_code"));
}

#[test]
fn http_response_grammar_keeps_its_status_code() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::EndpointBlock,
        "HTTP/1.1 404",
        None,
    )]);

    assert!(
        facts.iter().any(|fact| {
            fact.fact_kind.as_str() == "status_code" && fact.display_value == "404"
        })
    );
}

#[test]
fn auth_prose_does_not_create_protocol_facts() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::EndpointBlock,
        "Bearer token, Basic auth, and OAuth are mentioned in prose",
        None,
    )]);

    assert!(facts.iter().all(|fact| fact.fact_kind.as_str() != "auth_rule"));
}

#[test]
fn explicit_authorization_header_keeps_formal_auth_scheme() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::EndpointBlock,
        "Authorization: Bearer opaque-credential",
        None,
    )]);

    assert!(facts.iter().any(|fact| {
        fact.fact_kind.as_str() == "auth_rule" && fact.display_value == "bearer_token"
    }));
}

#[test]
fn extracts_code_identifiers_from_python_code_blocks() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::CodeBlock,
        "def build_router(state):\n    pass\n\nclass AppRouter:\n    pass",
        Some("python"),
    )]);

    let code_idents: Vec<_> =
        facts.iter().filter(|f| f.fact_kind.as_str() == "code_identifier").collect();

    assert!(
        code_idents.iter().any(|f| f.display_value == "build_router"),
        "expected CodeIdentifier for 'build_router', got: {:?}",
        code_idents.iter().map(|f| &f.display_value).collect::<Vec<_>>()
    );
}

#[test]
fn no_code_identifiers_from_unsupported_language() {
    // Rust is not yet supported by tree-sitter in our pipeline.
    // The heuristic fallback was removed — no guessing.
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::CodeBlock,
        "fn build_router(state: AppState) -> Router {",
        Some("rust"),
    )]);

    let code_idents: Vec<_> =
        facts.iter().filter(|f| f.fact_kind.as_str() == "code_identifier").collect();

    assert!(
        code_idents.is_empty(),
        "expected no CodeIdentifier for unsupported language, got: {:?}",
        code_idents.iter().map(|f| &f.display_value).collect::<Vec<_>>()
    );
}

#[test]
fn extracts_config_keys_from_json() {
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::CodeBlock,
        "{\"max_connections\": 200, \"shared_buffers\": \"256MB\"}",
        Some("json"),
    )]);

    let config_keys: Vec<_> =
        facts.iter().filter(|f| f.fact_kind.as_str() == "configuration_key").collect();

    assert!(!config_keys.is_empty(), "expected at least one ConfigurationKey fact, got none");
    let key_names: Vec<_> = config_keys.iter().map(|f| f.display_value.as_str()).collect();
    assert!(
        key_names.contains(&"max_connections"),
        "expected 'max_connections', got: {:?}",
        key_names
    );
}

#[test]
fn extracts_config_keys_from_yaml() {
    // YAML is now parsed structurally via serde_yaml.
    let facts = extract_facts(vec![make_test_block(
        StructuredBlockKind::CodeBlock,
        "max_connections: 200\nshared_buffers: 256MB",
        Some("yaml"),
    )]);

    let config_keys: Vec<_> =
        facts.iter().filter(|f| f.fact_kind.as_str() == "configuration_key").collect();

    assert!(!config_keys.is_empty(), "expected YAML config keys to be extracted, got none");
    let key_names: Vec<_> = config_keys.iter().map(|f| f.display_value.as_str()).collect();
    assert!(
        key_names.contains(&"max_connections"),
        "expected 'max_connections' from YAML, got: {:?}",
        key_names
    );
}
