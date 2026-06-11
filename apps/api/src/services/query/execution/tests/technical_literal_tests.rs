use super::*;
use crate::services::query::execution::preflight::select_preflight_literal_document_id;
use crate::services::query::execution::technical_literals::extract_package_command_literals;

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
    let mut ir =
        query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["route_map_inventory"]);
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
    let mut ir =
        query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["route_map_inventory"]);
    ir.act = QueryAct::RetrieveValue;
    ir.literal_constraints =
        vec![LiteralSpan { text: "alpha".to_string(), kind: LiteralKind::Identifier }];

    let intent = detect_technical_literal_intent_from_query_ir("What mentions alpha?", &ir);

    assert!(!intent.wants_parameters);
}

#[test]
fn structural_identifier_literal_requests_parameters() {
    let mut ir =
        query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["route_map_inventory"]);
    ir.act = QueryAct::RetrieveValue;
    ir.literal_constraints =
        vec![LiteralSpan { text: "callbackUrl".to_string(), kind: LiteralKind::Identifier }];

    let intent = detect_technical_literal_intent_from_query_ir("What is callbackUrl?", &ir);

    assert!(intent.wants_parameters);
}

#[test]
fn exact_literal_queries_without_technical_tag_still_build_technical_literal_sections() {
    let mut ir =
        query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["route_map_inventory"]);
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

#[test]
fn build_exact_technical_literals_section_groups_literals_by_document() {
    let section = build_exact_technical_literals_section(
            "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
            &query_ir_with_act_scope_and_target_types(
                QueryAct::RetrieveValue,
                QueryScope::MultiDocument,
                ["endpoint"],
            ),
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: Uuid::now_v7(),
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "To get the current Checkout Server status, send a GET request to /system/info.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.9),
                    source_text: repair_technical_layout_noise(
                        "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: Uuid::now_v7(),
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts returns the Rewards Service account list.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.8),
                    source_text: repair_technical_layout_noise(
                        "http://demo.local:8080/rewards-api/rest/v1/version\n/v1/accounts\nGET",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    let checkout_index =
        section.find("Document: `checkout_server_reference.pdf`").unwrap_or(usize::MAX);
    let rewards_index =
        section.find("Document: `rewards_service_reference.pdf`").unwrap_or(usize::MAX);
    let system_info_index = section.find("`/system/info`").unwrap_or(usize::MAX);
    let accounts_index = section.find("`/v1/accounts`").unwrap_or(usize::MAX);

    assert!(checkout_index < rewards_index);
    assert!(checkout_index < system_info_index);
    assert!(rewards_index < accounts_index);
    assert!(section.contains("current Checkout Server status"));
    assert!(section.contains("Rewards Service account list"));
}

#[test]
fn build_exact_technical_literals_section_prefers_question_matched_window_per_document() {
    let section = build_exact_technical_literals_section(
            "Which endpoint returns the Rewards Service account list?",
            &query_ir_with_act_scope_and_target_types(
                QueryAct::RetrieveValue,
                QueryScope::MultiDocument,
                ["endpoint"],
            ),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: Uuid::now_v7(),
                document_label: "rewards_service_reference.pdf".to_string(),
                excerpt: "GET /v1/accounts returns the Rewards Service account list.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "http://demo.local:8080/rewards-api/rest/v1/version\nGET\nRewards Service version\n/v1/accounts\nGET\nGet Rewards Service account list.",
                ),
            }],
        )
        .unwrap_or_default();

    assert!(section.contains("`/v1/accounts`"));
    assert!(!section.contains("`/rewards-api/rest/v1/version`"));
}

#[test]
fn build_exact_technical_literals_section_balances_documents_before_second_same_doc_chunk() {
    let rewards_document_id = Uuid::now_v7();
    let checkout_document_id = Uuid::now_v7();
    let section = build_exact_technical_literals_section(
            "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
            &query_ir_with_act_scope_and_target_types(
                QueryAct::RetrieveValue,
                QueryScope::MultiDocument,
                ["endpoint"],
            ),
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts returns the Rewards Service account list.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise("/v1/accounts\nGET\nGet Rewards Service account list."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/cards/bypage returns paginated Rewards Service card list.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise("/v1/cards/bypage\nGET\nGet paginated Rewards Service card list."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/cards returns all cards.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.97),
                    source_text: repair_technical_layout_noise("/v1/cards\nGET\nGet all cards."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "To get the current Checkout Server status, send a GET request to /system/info.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.6),
                    source_text: repair_technical_layout_noise("http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info"),
                },
            ],
        )
        .unwrap_or_default();

    assert!(section.contains("Document: `checkout_server_reference.pdf`"));
    assert!(section.contains("`/system/info`"), "{section}");
}

#[test]
fn build_exact_technical_literals_section_picks_best_matching_chunk_within_document() {
    let cash_document_id = Uuid::now_v7();
    let section = build_exact_technical_literals_section(
        "Which endpoint returns the current Checkout Server status?",
        &query_ir_with_literals_and_target_types(["current status checkout server"], ["endpoint"]),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: cash_document_id,
                document_label: "checkout_server_reference.pdf".to_string(),
                excerpt: "GET /cashes returns the register list.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text: repair_technical_layout_noise("/cashes\nGET\nGet register list."),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: cash_document_id,
                document_label: "checkout_server_reference.pdf".to_string(),
                excerpt:
                    "To get the current Checkout Server status, send a GET request to /system/info."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.7),
                source_text: repair_technical_layout_noise(
                    "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(section.contains("system/info"));
    assert!(!section.contains("`/cashes`"));
}

#[test]
fn build_exact_technical_literals_section_prefers_document_local_clause_in_multi_doc_question() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let checkout_list = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: checkout_document_id,
        document_label: "checkout_server_reference.pdf".to_string(),
        excerpt: "GET /cashes returns the register list.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.95),
        source_text: repair_technical_layout_noise("/cashes\nGET\nGet register list."),
    };
    let checkout_system_info = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: checkout_document_id,
        document_label: "checkout_server_reference.pdf".to_string(),
        excerpt: "To get the current Checkout Server status, send a GET request to /system/info."
            .to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.7),
        source_text: repair_technical_layout_noise(
            "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
        ),
    };
    let rewards_bypage = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: rewards_document_id,
        document_label: "rewards_service_reference.pdf".to_string(),
        excerpt: "GET /v1/accounts/bypage returns paginated Rewards Service accounts.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.95),
        source_text: repair_technical_layout_noise(
            "/v1/accounts/bypage\nGET\npageNumber\npageSize\nGet paginated Rewards Service accounts.",
        ),
    };
    let rewards_accounts = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: rewards_document_id,
        document_label: "rewards_service_reference.pdf".to_string(),
        excerpt: "GET /v1/accounts returns accounts without pagination.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.7),
        source_text: repair_technical_layout_noise(
            "/v1/accounts\nGET\nGet Rewards Service account list.",
        ),
    };
    let question = "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?";
    let section = build_exact_technical_literals_section(
        question,
        &query_ir_with_scope_literals_and_target_types(
            QueryScope::MultiDocument,
            ["current status checkout server", "account list rewards service"],
            ["endpoint"],
        ),
        &[checkout_list, checkout_system_info, rewards_bypage, rewards_accounts],
    )
    .unwrap_or_default();

    assert!(section.contains("Document: `checkout_server_reference.pdf`"));
    assert!(section.contains("Document: `rewards_service_reference.pdf`"));
    assert!(section.contains("`/system/info`"));
    assert!(!section.contains("`/cashes`"));
    assert!(section.contains("`/v1/accounts`"));
    assert!(!section.contains("`/v1/accounts/bypage`"));
}

#[test]
fn build_exact_technical_literals_section_prefers_cash_current_info_clause_over_generic_cash_list()
{
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let checkout_clients = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: checkout_document_id,
        document_label: "checkout_server_reference.pdf".to_string(),
        excerpt: "GET /checkout-api/rest/dictionaries/clients returns Checkout Server client list."
            .to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.92),
        source_text: repair_technical_layout_noise(
            "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/clients\nGet Checkout Server client list.",
        ),
    };
    let checkout_system_info = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: checkout_document_id,
        document_label: "checkout_server_reference.pdf".to_string(),
        excerpt: "To get the current Checkout Server status, send a GET request to /system/info."
            .to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.71),
        source_text: repair_technical_layout_noise(
            "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nGet current Checkout Server status.",
        ),
    };
    let rewards_accounts = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: rewards_document_id,
        document_label: "rewards_service_reference.pdf".to_string(),
        excerpt: "GET /v1/accounts returns the Rewards Service account list.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.94),
        source_text: repair_technical_layout_noise(
            "/v1/accounts\nGET\nGet Rewards Service account list.",
        ),
    };
    let section = build_exact_technical_literals_section(
            "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
            &query_ir_with_scope_literals_and_target_types(
                QueryScope::MultiDocument,
                ["current status checkout server", "account list rewards service"],
                ["endpoint"],
            ),
            &[rewards_accounts, checkout_clients, checkout_system_info],
        )
        .unwrap_or_default();

    assert!(section.contains("`/system/info`"));
    assert!(!section.contains("`/checkout-api/rest/dictionaries/clients`"));
    assert!(section.contains("`/v1/accounts`"));
}

#[test]
fn technical_literal_focus_keyword_segments_splits_english_multi_clause_questions() {
    let segments = technical_literal_focus_keyword_segments(
        "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
        None,
    );

    assert!(segments.len() >= 2);
    assert!(segments.iter().any(|segment| segment.iter().any(|keyword| keyword == "rewards")));
    assert!(segments.iter().any(|segment| segment.iter().any(|keyword| keyword == "profile")));
}

#[test]
fn technical_literal_focus_keyword_segments_keep_multi_document_literals_separate() {
    let segments = technical_literal_focus_keyword_segments(
        "Which endpoints cover the Alpha status and Beta account list?",
        Some(&query_ir_with_scope_literals_and_target_types(
            QueryScope::MultiDocument,
            ["Alpha status", "Beta account list"],
            ["endpoint"],
        )),
    );

    assert_eq!(segments.len(), 2);
    assert!(segments[0].contains(&"alpha".to_string()));
    assert!(!segments[0].contains(&"beta".to_string()));
    assert!(segments[1].contains(&"beta".to_string()));
    assert!(!segments[1].contains(&"alpha".to_string()));
}

#[test]
fn technical_literal_focus_keywords_with_ir_literal_prioritizes_literal_tokens() {
    // Compiler literals are emitted first, but surrounding request tokens
    // still participate so nearby config blocks can be disambiguated without
    // relying on a language-specific stop list.
    let ir = query_ir_with_literals_and_target_types(["Acme Control Center"], ["port"]);
    let keywords = technical_literal_focus_keywords(
        "What port does the Acme Control Center use in production?",
        Some(&ir),
    );

    assert!(keywords.iter().any(|keyword| keyword == "control"));
    assert!(keywords.iter().any(|keyword| keyword == "center"));
    assert!(keywords.iter().any(|keyword| keyword == "production"));
    assert!(
        keywords.iter().position(|keyword| keyword == "control").unwrap()
            < keywords.iter().position(|keyword| keyword == "production").unwrap()
    );
}

#[test]
fn technical_literal_focus_keywords_without_literals_keeps_all_question_tokens_above_floor() {
    // Without literal constraints the helper keeps every >=4-char token
    // from the question verbatim. Previously a hard-coded stop list
    // dropped framing words like "which" / "endpoint"; that list is
    // gone and downstream ranking is expected to weigh tokens by how
    // often they appear in candidate documents instead.
    let keywords = technical_literal_focus_keywords(
        "Which endpoint returns the account list?",
        Some(&generic_query_ir()),
    );

    assert!(keywords.iter().any(|keyword| keyword == "which"));
    assert!(keywords.iter().any(|keyword| keyword == "endpoint"));
    assert!(keywords.iter().any(|keyword| keyword == "returns"));
    assert!(keywords.iter().any(|keyword| keyword == "account"));
    // Tokens shorter than 4 characters are still filtered out — that
    // floor is structural, not a legacy stop list.
    assert!(!keywords.iter().any(|keyword| keyword.chars().count() < 4));
}

#[test]
fn expanded_candidate_limit_prefers_deeper_combined_mode_search() {
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, true, 24), 24);
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Mix, 10, true, 24), 30);
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Document, 8, true, 24), 8);
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, false, 24), 24);
}

#[test]
fn technical_literal_candidate_limit_expands_document_recall_for_endpoint_questions() {
    assert_eq!(
        technical_literal_candidate_limit(
            TechnicalLiteralIntent {
                wants_urls: true,
                wants_paths: true,
                wants_methods: true,
                ..TechnicalLiteralIntent::default()
            },
            8,
        ),
        32
    );
    assert_eq!(
        technical_literal_candidate_limit(
            TechnicalLiteralIntent { wants_parameters: true, ..TechnicalLiteralIntent::default() },
            8,
        ),
        24
    );
    assert_eq!(
        technical_literal_candidate_limit(
            detect_technical_literal_intent("Tell me briefly what the library is about."),
            8,
        ),
        8
    );
}

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
    let text = "Install the connector with aptitude install alpha-connector before running dpkg-reconfigure alpha-connector.";

    assert_eq!(extract_package_command_literals(text, 4), vec!["alpha-connector".to_string()]);
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
    let selected = select_technical_literal_chunks(
        question,
        &ir,
        &[
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
                excerpt: "Inventory SOAP canonical WSDL.".to_string(),
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
        TechnicalLiteralIntent { wants_parameters: true, ..TechnicalLiteralIntent::default() },
        8,
        &technical_literal_focus_keywords(question, Some(&ir)),
        &[],
        false,
    );

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

    let selected = select_technical_literal_chunks(
        question,
        &ir,
        &[
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
        TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        8,
        &technical_literal_focus_keywords(question, Some(&ir)),
        &[generic_document_id],
        false,
    );

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

    let selected = select_technical_literal_chunks(
        question,
        &ir,
        &[
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
        TechnicalLiteralIntent { wants_parameters: true, ..TechnicalLiteralIntent::default() },
        8,
        &technical_literal_focus_keywords(question, Some(&ir)),
        &[generic_document_id],
        false,
    );

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
    let selected = select_technical_literal_chunks(
        question,
        &ir,
        &[
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
        TechnicalLiteralIntent {
            wants_urls: true,
            wants_paths: true,
            wants_methods: true,
            ..TechnicalLiteralIntent::default()
        },
        8,
        &technical_literal_focus_keywords(question, Some(&ir)),
        &[],
        false,
    );

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == inventory_document_id));
}
