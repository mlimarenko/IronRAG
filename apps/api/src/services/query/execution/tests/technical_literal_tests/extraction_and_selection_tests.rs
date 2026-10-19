use super::*;

#[test]
fn literal_extractors_normalize_markdown_wrapped_tokens() {
    let text = "Method: `GET` Path: `/system/info` WSDL: `http://demo.local:8080/inventory-api/ws/inventory.wsdl` Param: `withCards`";

    assert_eq!(extract_http_methods(text, 2), vec!["GET".to_string()]);
    assert_eq!(extract_explicit_path_literals(text, 2), vec!["/system/info".to_string()]);
    assert_eq!(
        extract_url_literals(text, 2),
        vec!["http://demo.local:8080/inventory-api/ws/inventory.wsdl".to_string()]
    );
    assert_eq!(extract_parameter_literals(text, 2), vec!["withCards".to_string()]);
}

#[test]
fn package_literal_extractor_reads_inline_install_commands() {
    let text = "Install the connector with sample-install alpha-connector before running sample-configure alpha-connector.";

    assert_eq!(extract_package_command_literals(text, 4), vec!["alpha-connector".to_string()]);
}

#[test]
fn package_literal_extractor_reads_common_install_command_shapes() {
    let text = "sample-install beta-agent\nsample-add gamma-agent\nsample-install @scope/delta\nsample-add epsilon-ui";

    assert_eq!(
        extract_package_command_literals(text, 8),
        vec![
            "beta-agent".to_string(),
            "gamma-agent".to_string(),
            "@scope/delta".to_string(),
            "epsilon-ui".to_string()
        ]
    );
}

#[test]
fn parameter_literal_extractor_reads_query_assignment_names_without_prefix_lists() {
    let text = "Query parameters: ?cursor=<opaque_string> and ?limit=<int>.";

    assert_eq!(
        extract_parameter_literals(text, 4),
        vec!["cursor".to_string(), "limit".to_string()]
    );
}

#[test]
fn preflight_literal_document_selection_prefers_chunk_signal_over_generic_title_overlap() {
    let question = "Which commands and settings configure the scan folder through RareProtocol?";
    let generic_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let ir = query_ir_with_literals_and_target_types(
        ["scan folder through RareProtocol"],
        ["path", "config_key", "protocol"],
    );
    let selected = select_preflight_literal_document_id(
        question,
        &ir,
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: generic_document_id,
                document_label: "scan folder settings checklist.md".to_string(),
                excerpt: "Generic setup checklist. Save the form after changing terminal labels."
                    .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.50),
                source_text:
                    "Generic setup checklist. Save the form after changing terminal labels."
                        .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: target_document_id,
                document_label: "linux-workstation-sharing.pdf".to_string(),
                excerpt:
                    "RareProtocol scan folder setup uses /srv/scans and scan_share = writable."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.90),
                source_text:
                    "RareProtocol scan folder setup uses /srv/scans and scan_share = writable."
                        .to_string(),
            },
        ],
    );

    assert_eq!(selected, Some(target_document_id));
}

#[test]
fn select_technical_literal_chunks_focuses_single_source_parameter_question_on_best_document() {
    let question = "What is the pageNumber parameter called in the pagination API?";
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let ir = query_ir_with_literals_and_target_types(["pageNumber"], ["parameter"]);
    let selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &ir,
        chunks: &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: rewards_document_id,
                document_label: "rewards_accounts_api_contract.md".to_string(),
                excerpt: "| `pageNumber` | 1-based page number |".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "| `pageNumber` | 1-based page number |",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "Inventory SOAP primary WSDL.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.98),
                source_text: repair_technical_layout_noise(
                    "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "SOAP over HTTP.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.97),
                source_text: "SOAP over HTTP.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "Agents use XML.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.96),
                source_text: "Agents use XML.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "Port 8080.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text: "Port 8080.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "Contract note.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.94),
                source_text: "Contract note.".to_string(),
            },
        ],
        technical_literal_intent: TechnicalLiteralIntent {
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        top_k: 8,
        literal_focus_keywords: &technical_literal_focus_keywords(question, Some(&ir)),
        preferred_document_ids: &[],
        pagination_requested: false,
    });

    assert_eq!(selected.len(), 1);
    assert!(selected.iter().all(|chunk| chunk.document_id == rewards_document_id));
    assert!(selected.iter().all(|chunk| chunk.source_text.contains("pageNumber")));
    assert!(!selected.iter().any(|chunk| chunk.document_id == inventory_document_id));
}

#[test]
fn select_technical_literal_chunks_prefers_setup_focus_over_generic_graph_preference() {
    let question =
        "How do I configure Provider Alpha: package, config file, and parameters to check?";
    let generic_document_id = Uuid::now_v7();
    let setup_document_id = Uuid::now_v7();
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["configuration_file", "package", "config_key", "parameter"],
    );
    ir.document_focus = Some(DocumentHint { hint: "Provider Alpha".to_string() });

    let selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &ir,
        chunks: &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: generic_document_id,
                document_label: "Provider Alpha overview".to_string(),
                excerpt: "Example for Provider Alpha installs alpha-generic-bridge.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
                score: Some(100.0),
                source_text: repair_technical_layout_noise(
                    "Example for Provider Alpha. Install alpha-generic-bridge before opening the payment screen.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: setup_document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt:
                    "Install alpha-provider-module and configure /opt/provider-alpha/alpha.conf."
                        .to_string(),
                score_kind:
                    crate::services::query::execution::RuntimeChunkScoreKind::DocumentIdentity,
                score: Some(90.0),
                source_text: repair_technical_layout_noise(
                    "Install alpha-provider-module. Main file: /opt/provider-alpha/alpha.conf",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 1,
                chunk_kind: Some("table_row".to_string()),
                document_id: setup_document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Parameter clientId identifies the tenant.".to_string(),
                score_kind:
                    crate::services::query::execution::RuntimeChunkScoreKind::DocumentIdentity,
                score: Some(80.0),
                source_text: repair_technical_layout_noise(
                    "Name: clientId | Type: string | Default: empty",
                ),
            },
        ],
        technical_literal_intent: TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        top_k: 8,
        literal_focus_keywords: &technical_literal_focus_keywords(question, Some(&ir)),
        preferred_document_ids: &[generic_document_id],
        pagination_requested: false,
    });

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == setup_document_id));
    assert!(selected.iter().any(|chunk| chunk.source_text.contains("alpha-provider-module")));
    assert!(selected.iter().any(|chunk| chunk.source_text.contains("clientId")));
    assert!(!selected.iter().any(|chunk| chunk.source_text.contains("alpha-generic-bridge")));
}

#[test]
fn select_technical_literal_chunks_keeps_setup_focus_for_package_only_configure_ir() {
    let question = "How do I configure Provider Alpha: package and parameters?";
    let generic_document_id = Uuid::now_v7();
    let setup_document_id = Uuid::now_v7();
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["package", "parameter"],
    );
    ir.document_focus = Some(DocumentHint { hint: "Provider Alpha".to_string() });

    let selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &ir,
        chunks: &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: generic_document_id,
                document_label: "Payment examples".to_string(),
                excerpt: "Provider Alpha quick example uses alpha-generic-bridge.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
                score: Some(100.0),
                source_text: repair_technical_layout_noise(
                    "Provider Alpha quick example uses alpha-generic-bridge and option genericMode.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: setup_document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Install alpha-provider-module.".to_string(),
                score_kind:
                    crate::services::query::execution::RuntimeChunkScoreKind::DocumentIdentity,
                score: Some(90.0),
                source_text: repair_technical_layout_noise(
                    "Install alpha-provider-module. Name: partnerId | Name: clientSecret",
                ),
            },
        ],
        technical_literal_intent: TechnicalLiteralIntent {
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        top_k: 8,
        literal_focus_keywords: &technical_literal_focus_keywords(question, Some(&ir)),
        preferred_document_ids: &[generic_document_id],
        pagination_requested: false,
    });

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == setup_document_id));
    assert!(selected.iter().any(|chunk| chunk.source_text.contains("alpha-provider-module")));
    assert!(!selected.iter().any(|chunk| chunk.source_text.contains("alpha-generic-bridge")));
}

#[test]
fn select_technical_literal_chunks_prefers_matching_wsdl_document_for_single_source_question() {
    let question = "Which WSDL does the inventory SOAP API use?";
    let checkout_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let ir = query_ir_with_literals_and_target_types(["inventory soap api"], ["endpoint", "wsdl"]);
    let selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &ir,
        chunks: &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: checkout_document_id,
                document_label: "checkout_runtime_contract.md".to_string(),
                excerpt: "Checkout runtime notes.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "Checkout Runtime Contract\nRuntime notes.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.97),
                source_text: repair_technical_layout_noise(
                    "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                ),
            },
        ],
        technical_literal_intent: TechnicalLiteralIntent {
            wants_urls: true,
            wants_paths: true,
            wants_methods: true,
            ..TechnicalLiteralIntent::default()
        },
        top_k: 8,
        literal_focus_keywords: &technical_literal_focus_keywords(question, Some(&ir)),
        preferred_document_ids: &[],
        pagination_requested: false,
    });

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == inventory_document_id));
}
