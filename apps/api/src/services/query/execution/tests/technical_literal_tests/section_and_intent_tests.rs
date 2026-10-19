use super::*;

#[test]
fn build_exact_technical_literals_section_extracts_urls_paths_and_parameters() {
    let section = build_exact_technical_literals_section(
            "What pagination parameters and URL are used?",
            &query_ir_with_scope_and_target_types(
                QueryScope::SingleDocument,
                ["endpoint", "parameter"],
            ),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: Uuid::now_v7(),
                document_label: "api.pdf".to_string(),
                excerpt: "Retrieve accounts list by page.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "http\n://demo.local:8080/rewards-api/rest/v1/accounts\n/bypage\npageNu\nmber\npageSize\nwithCar\nds\nnumber\n_starting",
                ),
            }],
        )
        .unwrap_or_default();

    assert!(section.contains("Document: `api.pdf`"));
    assert!(section.contains("Matched excerpt: Retrieve accounts list by page."));
    assert!(section.contains("`http://demo.local:8080/rewards-api/rest/v1/accounts/bypage`"));
    assert!(
        section.contains("`/v1/accounts/bypage`")
            || section.contains("`/rewards-api/rest/v1/accounts/bypage`")
    );
    assert!(section.contains("`pageNumber`"));
    assert!(section.contains("`pageSize`"));
    assert!(section.contains("`withCards`"));
    assert!(section.contains("`number_starting`"));
}

#[test]
fn build_exact_technical_literals_section_renders_line_item_inventory() {
    let section = build_exact_technical_literals_section(
        "How do I configure Provider Alpha parameters?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::ConfigureHow,
            QueryScope::SingleDocument,
            ["configuration_file", "config_key"],
        ),
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("table_row".to_string()),
            document_id: Uuid::now_v7(),
            document_label: "Provider Alpha setup".to_string(),
            excerpt: "Parameters alphaUrl and alphaTimeout configure Provider Alpha.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.9),
            source_text: repair_technical_layout_noise(
                "Path: /opt/provider-alpha/alpha.conf\nName: alphaUrl\nName: alphaTimeout",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(section.contains("  Paths:\n    - `/opt/provider-alpha/alpha.conf`"));
    assert!(section.contains("  Parameters:\n    - `alphaUrl`\n    - `alphaTimeout`"));
}

#[test]
fn config_section_literal_extractor_keeps_identifier_shaped_sections() {
    let sections = extract_config_section_literals(
        "`[CFG]`\n[UI.ScanPanel.qrCode]\narr[0]\n[text](http://demo.local)\nvalue = document.shopCode[04d]\n[9]\n[bad value]",
        8,
    );

    assert_eq!(sections, vec!["[CFG]".to_string(), "[UI.ScanPanel.qrCode]".to_string()]);
}

#[test]
fn parameter_literal_extractor_keeps_spaced_config_assignment_names() {
    let parameters = extract_parameter_literals(
        "[UI.ScanPanel.qrCode]\nvisible = true\narr[0]\nplain prose without assignment",
        8,
    );

    assert_eq!(parameters, vec!["visible".to_string()]);
}

#[test]
fn build_exact_technical_literals_section_renders_config_sections() {
    let section = build_exact_technical_literals_section(
        "How do I configure Provider Alpha primary and display sections?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::ConfigureHow,
            QueryScope::SingleDocument,
            ["configuration_file", "config_key"],
        ),
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: Uuid::now_v7(),
            document_label: "Provider Alpha setup".to_string(),
            excerpt: "Provider Alpha uses the primary configuration section.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.9),
            source_text: repair_technical_layout_noise(
                "/opt/provider-alpha/alpha.conf\n[CFG]\nendpointUrl = http://demo.local\n[UI.ScanPanel.qrCode]\nvisible = true",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(section.contains("  Sections:\n    - `[CFG]`\n    - `[UI.ScanPanel.qrCode]"));
    assert!(section.contains("`endpointUrl`"));
    assert!(section.contains("`visible`"));
}

#[test]
fn route_target_types_do_not_expand_endpoint_literal_intent() {
    let intent = detect_technical_literal_intent_from_query_ir(
        "Which graph route connects the entities?",
        &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["relationship"]),
    );

    assert!(!intent.wants_urls);
    assert!(!intent.wants_paths);
    assert!(!intent.wants_methods);
}

#[test]
fn module_package_and_config_targets_request_exact_literal_context() {
    let module_intent = detect_technical_literal_intent_from_query_ir(
        "Which module should be installed for Provider Alpha?",
        &query_ir_with_scope_and_target_types(
            QueryScope::SingleDocument,
            ["software_module", "package"],
        ),
    );

    assert!(module_intent.wants_parameters);
    assert!(module_intent.wants_paths);

    let config_intent = detect_technical_literal_intent_from_query_ir(
        "Which configuration file and keys configure Provider Alpha?",
        &query_ir_with_scope_and_target_types(
            QueryScope::SingleDocument,
            ["configuration_file", "filesystem_path", "config_key"],
        ),
    );

    assert!(config_intent.wants_paths);
    assert!(config_intent.wants_parameters);
    assert!(!config_intent.wants_methods);
}

#[test]
fn configure_how_requests_setup_literal_context_without_specific_target_tags() {
    let intent = detect_technical_literal_intent_from_query_ir(
        "How should Provider Alpha be configured?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::ConfigureHow,
            QueryScope::SingleDocument,
            ["organization", "procedure"],
        ),
    );

    assert!(intent.wants_paths);
    assert!(intent.wants_parameters);
    assert!(!intent.wants_methods);
}

#[test]
fn configure_how_follow_up_with_anchor_requests_setup_literal_context() {
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["organization", "procedure"],
    );
    ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "Provider Alpha".to_string(),
    }];

    let intent = detect_technical_literal_intent_from_query_ir("Provider Alpha", &ir);

    assert!(intent.wants_paths);
    assert!(intent.wants_parameters);
}

#[test]
fn configure_how_follow_up_keeps_setup_literals_across_neighbor_chunks() {
    let document_id = Uuid::now_v7();
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["organization", "procedure"],
    );
    ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "Provider Alpha".to_string(),
    }];

    let section = build_exact_technical_literals_section(
        "show the configuration file",
        &ir,
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 1,
                chunk_kind: Some("paragraph".to_string()),
                document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Install package alpha-provider-module.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.94),
                source_text: repair_technical_layout_noise(
                    "Install package alpha-provider-module before editing /opt/provider-alpha/alpha.conf.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 2,
                chunk_kind: Some("table_row".to_string()),
                document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Parameter alphaMerchantId is required for Provider Alpha.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.71),
                source_text: repair_technical_layout_noise(
                    "Name: alphaMerchantId | Type: string | Default: empty",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(section.contains("`alpha-provider-module`"), "{section}");
    assert!(section.contains("`/opt/provider-alpha/alpha.conf`"), "{section}");
    assert!(section.contains("`alphaMerchantId`"), "{section}");
}

#[test]
fn build_exact_technical_literals_section_keeps_neighbor_config_chunks_for_single_document() {
    let document_id = Uuid::now_v7();
    let section = build_exact_technical_literals_section(
        "How do I configure Provider Alpha package and configuration file?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::ConfigureHow,
            QueryScope::SingleDocument,
            ["configuration_file", "software_module", "package"],
        ),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 2,
                chunk_kind: Some("table_row".to_string()),
                document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Parameter enableAlpha controls Provider Alpha.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text: repair_technical_layout_noise(
                    "Sheet: Configuration | Row 1 | Name: enableAlpha | Default: true",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 1,
                chunk_kind: Some("paragraph".to_string()),
                document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Install alpha-provider-module before configuration.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.7),
                source_text: repair_technical_layout_noise(
                    "Install package alpha-provider-module. Configure /opt/provider-alpha/alpha.conf in section [Main].",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(section.contains("Document: `Provider Alpha setup`"));
    assert!(section.contains("`enableAlpha`"), "{section}");
    assert!(section.contains("`alpha-provider-module`"), "{section}");
    assert!(section.contains("`/opt/provider-alpha/alpha.conf`"), "{section}");
}

#[test]
fn build_exact_technical_literals_section_keeps_config_chunks_for_configure_scope() {
    let document_id = Uuid::now_v7();
    let section = build_exact_technical_literals_section(
        "How do I configure Provider Alpha package, file, and parameters?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::ConfigureHow,
            QueryScope::MultiDocument,
            [
                "configuration_file",
                "software_module",
                "package",
                "config_key",
            ],
        ),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: Some("paragraph".to_string()),
                document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Install package alpha-provider-module.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.98),
                source_text: repair_technical_layout_noise(
                    "Install package alpha-provider-module. Main file: /opt/provider-alpha/alpha.conf",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 1,
                chunk_kind: Some("table_row".to_string()),
                document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Parameter fillDetails controls payment details.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.91),
                source_text: repair_technical_layout_noise(
                    "Name: fillDetails | Type: boolean | Default: true",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 2,
                chunk_kind: Some("table_row".to_string()),
                document_id,
                document_label: "Provider Alpha setup".to_string(),
                excerpt: "Parameter partnerId identifies the merchant.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "Name: partnerId | Type: string | Default: empty",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(section.contains("`alpha-provider-module`"), "{section}");
    assert!(section.contains("`/opt/provider-alpha/alpha.conf`"), "{section}");
    assert!(section.contains("`fillDetails`"), "{section}");
    assert!(section.contains("`partnerId`"), "{section}");
}

#[test]
fn detect_technical_literal_intent_falls_back_to_parameters_for_exact_literal_queries_without_known_tags()
 {
    let mut ir = query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["record"]);
    ir.act = QueryAct::RetrieveValue;
    ir.literal_constraints =
        vec![LiteralSpan { text: "12".to_string(), kind: LiteralKind::NumericCode }];

    let intent = detect_technical_literal_intent_from_query_ir(
        "What is the inventory route_map value?",
        &ir,
    );

    assert!(intent.wants_parameters);
}

#[test]
fn plain_alphabetic_identifier_literal_does_not_request_parameters() {
    let mut ir = query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["record"]);
    ir.act = QueryAct::RetrieveValue;
    ir.literal_constraints =
        vec![LiteralSpan { text: "alpha".to_string(), kind: LiteralKind::Identifier }];

    let intent = detect_technical_literal_intent_from_query_ir("What mentions alpha?", &ir);

    assert!(!intent.wants_parameters);
}

#[test]
fn structural_identifier_literal_requests_parameters() {
    let mut ir = query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["record"]);
    ir.act = QueryAct::RetrieveValue;
    ir.literal_constraints =
        vec![LiteralSpan { text: "callbackUrl".to_string(), kind: LiteralKind::Identifier }];

    let intent = detect_technical_literal_intent_from_query_ir("What is callbackUrl?", &ir);

    assert!(intent.wants_parameters);
}

#[test]
fn exact_literal_queries_without_technical_tag_still_build_technical_literal_sections() {
    let mut ir = query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["record"]);
    ir.act = QueryAct::RetrieveValue;
    ir.literal_constraints =
        vec![LiteralSpan { text: "12".to_string(), kind: LiteralKind::NumericCode }];
    let section = build_exact_technical_literals_section(
        "What is the inventory route_map value?",
        &ir,
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: Uuid::now_v7(),
            document_label: "inventory_reference.md".to_string(),
            excerpt: "Inventory route map is computed from route_map_key.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.9),
            source_text: repair_technical_layout_noise(
                "route_map_inventory_timeout_ms = 30000\nroute_map_inventory_retries = 5",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(section.contains("Document: `inventory_reference.md`"));
    assert!(section.contains("Parameters"));
    assert!(section.contains("route_map_inventory_timeout_ms"));
}

#[test]
fn focused_document_answer_intent_ignores_spurious_path_literals() {
    let mut query_ir =
        query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["secondary_heading"]);
    query_ir.target_entities = vec![EntityMention {
        label: "runtime PDF upload check".to_string(),
        role: EntityRole::Object,
    }];
    query_ir.literal_constraints = vec![LiteralSpan {
        text: "upload://upload_smoke_fixture.docx".to_string(),
        kind: LiteralKind::Path,
    }];

    let intent = detect_technical_literal_intent_from_query_ir(
        "What report name appears in the runtime PDF upload check?",
        &query_ir,
    );

    assert!(!intent.wants_urls);
    assert!(!intent.wants_paths);
    assert!(!intent.wants_methods);
    assert!(!intent.wants_parameters);
}

#[test]
fn build_exact_technical_literals_section_extracts_dotted_config_keys_and_values() {
    let section = build_exact_technical_literals_section(
        "Which naming strategy parameters are used for implicit and physical modes?",
        &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["config_key"]),
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: Uuid::now_v7(),
            document_label: "alpha-service.md".to_string(),
            excerpt: "Alpha service setup uses an application properties file.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.9),
            source_text: repair_technical_layout_noise(
                "alpha.datasource.url=jdbc:demo://127.0.0.1/main\n\
alpha.naming.implicit-strategy=com.example.ImplicitMode\n\
alpha.naming.physical-strategy=com.example.PhysicalMode\n\
alpha.batch.size=100",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(section.contains("`alpha.naming.implicit-strategy`"));
    assert!(section.contains("`com.example.ImplicitMode`"));
    assert!(section.contains("`alpha.naming.physical-strategy`"));
    assert!(section.contains("`com.example.PhysicalMode`"));
    assert!(section.contains("Focused literal excerpt:"));
}
