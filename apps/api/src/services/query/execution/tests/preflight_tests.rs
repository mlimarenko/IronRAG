use super::*;
use crate::services::query::execution::preflight::{
    extend_setup_preflight_chunks_from_structured_context, merge_setup_preflight_structured_blocks,
    question_prefers_single_exact_literal_scope,
};

#[test]
fn canonical_preflight_answer_prefers_missing_explicit_document_before_other_paths() {
    let missing_document_id = Uuid::now_v7();
    let available_document_id = Uuid::now_v7();
    let document_index = HashMap::from([(
        available_document_id,
        sample_document_row_for_preflight(available_document_id, "available.md"),
    )]);

    let answer = build_canonical_preflight_answer(
        "What does missing-contract.md say?",
        &generic_query_ir(),
        &QueryIntentProfile::default(),
        &document_index,
        Some("table answer".to_string()),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: missing_document_id,
            document_label: "available.md".to_string(),
            excerpt: "Available document content.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.9),
            source_text: "Available document content.".to_string(),
        }],
    )
    .expect("missing explicit document answer");

    assert!(answer.contains("missing-contract.md"));
}

#[test]
fn canonical_preflight_answer_reuses_single_endpoint_override_for_live_path() {
    let document_id = Uuid::now_v7();
    let document_index = HashMap::from([(
        document_id,
        sample_document_row_for_preflight(document_id, "checkout_runtime_contract.md"),
    )]);
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "checkout_runtime_contract.md".to_string(),
        excerpt: "Send a GET request to /system/info to fetch the current checkout server info."
            .to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.97),
        source_text: repair_technical_layout_noise(
            "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
        ),
    }];

    let revision_id = Uuid::now_v7();
    let answer = build_canonical_preflight_answer(
        "Which endpoint returns the current checkout server info?",
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["current info", "checkout server"],
            ["endpoint"],
        ),
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &document_index,
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "endpoint_path".to_string(),
                canonical_value_text: "/system/info".to_string(),
                canonical_value_exact: "/system/info".to_string(),
                canonical_value_json: json!("/system/info"),
                display_value: "/system/info".to_string(),
                qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &chunks,
    )
    .expect("single endpoint preflight answer");

    assert_eq!(answer, "The endpoint is `GET /system/info`.");
}

#[test]
fn build_preflight_answer_chunks_prioritizes_technical_literal_candidates() {
    let document_id = Uuid::now_v7();
    let noisy_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "checkout_runtime_contract.md".to_string(),
        excerpt: "The checkout server exposes runtime metadata.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.55),
        source_text: "Checkout runtime contract overview without the exact endpoint literal."
            .to_string(),
    };
    let endpoint_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "checkout_runtime_contract.md".to_string(),
        excerpt: "GET /system/info returns checkout server information.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.99),
        source_text: repair_technical_layout_noise(
            "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
        ),
    };

    let query_ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["current information", "checkout server"],
        ["endpoint"],
    );
    let merged = build_preflight_answer_chunks(
        "Which endpoint returns the current checkout server info?",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        std::slice::from_ref(&noisy_chunk),
        std::slice::from_ref(&endpoint_chunk),
    );
    let document_index = HashMap::from([(
        document_id,
        sample_document_row_for_preflight(document_id, "checkout_runtime_contract.md"),
    )]);
    let revision_id = Uuid::now_v7();
    let answer = build_canonical_preflight_answer(
        "Which endpoint returns the current checkout server info?",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &document_index,
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "endpoint_path".to_string(),
                canonical_value_text: "/system/info".to_string(),
                canonical_value_exact: "/system/info".to_string(),
                canonical_value_json: json!("/system/info"),
                display_value: "/system/info".to_string(),
                qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &merged,
    )
    .expect("single endpoint preflight answer from merged candidates");

    assert_eq!(answer, "The endpoint is `GET /system/info`.");
}

#[test]
fn setup_preflight_extends_override_context_with_late_parameter_chunks() {
    let setup_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunk = |document_id: Uuid, index: i32, text: &str| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id,
        chunk_index: index,
        chunk_kind: None,
        document_id,
        document_label: if document_id == setup_document_id {
            "provider-alpha-setup.md".to_string()
        } else {
            "provider-beta-setup.md".to_string()
        },
        excerpt: text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.9),
        source_text: repair_technical_layout_noise(text),
    };
    let mut preflight_chunks = vec![chunk(
        setup_document_id,
        0,
        "Install module with aptitude install alpha-connector and configure /opt/alpha/alpha.conf.",
    )];
    let duplicate_id = preflight_chunks[0].chunk_id;
    let mut duplicate = preflight_chunks[0].clone();
    duplicate.chunk_id = duplicate_id;
    let structured_context = vec![
        duplicate,
        chunk(setup_document_id, 6, "| fillPaymentDetails | true | Fill payment details |"),
        chunk(other_document_id, 1, "| foreignParameter | true | Noise |"),
    ];

    extend_setup_preflight_chunks_from_structured_context(
        &mut preflight_chunks,
        &structured_context,
        Some(&HashSet::from([setup_document_id])),
    );

    assert_eq!(preflight_chunks.len(), 2);
    assert!(
        preflight_chunks.iter().any(|chunk| chunk.source_text.contains("fillPaymentDetails")),
        "{preflight_chunks:#?}"
    );
    assert!(
        !preflight_chunks.iter().any(|chunk| chunk.source_text.contains("foreignParameter")),
        "{preflight_chunks:#?}"
    );
}

#[test]
fn setup_preflight_evidence_adds_late_structured_parameter_rows_inside_scope() {
    let setup_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut table_row = sample_structured_block_row(Uuid::now_v7(), setup_document_id, revision_id);
    table_row.block_kind = "table_row".to_string();
    table_row.ordinal = 48;
    table_row.text =
        "Sheet: Connector settings | Row 12 | Name: fillDetails | Type: boolean | Values: true false"
            .to_string();
    table_row.normalized_text = table_row.text.clone();
    let mut code_block =
        sample_structured_block_row(Uuid::now_v7(), setup_document_id, revision_id);
    code_block.block_kind = "code_block".to_string();
    code_block.ordinal = 50;
    code_block.text = "[Main]\n; Fill details\nfillDetails = true".to_string();
    code_block.normalized_text = code_block.text.clone();
    let mut unrelated =
        sample_structured_block_row(Uuid::now_v7(), other_document_id, Uuid::now_v7());
    unrelated.block_kind = "table_row".to_string();
    unrelated.text = "Sheet: Other settings | Name: foreignSecret | Type: string".to_string();
    unrelated.normalized_text = unrelated.text.clone();
    let mut evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };

    let added = merge_setup_preflight_structured_blocks(
        &mut evidence,
        setup_document_id,
        vec![unrelated, code_block, table_row],
        16,
    );

    assert_eq!(added, 2);
    let text =
        evidence.structured_blocks.iter().map(|block| block.text.as_str()).collect::<Vec<_>>();
    assert!(text.iter().any(|value| value.contains("fillDetails")), "{text:#?}");
    assert!(!text.iter().any(|value| value.contains("foreignSecret")), "{text:#?}");
    assert_eq!(
        evidence.structured_blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>(),
        vec![48, 50]
    );
}

#[test]
fn canonical_preflight_answer_keeps_follow_up_setup_questions_generative() {
    let document_id = Uuid::now_v7();
    let document_index = HashMap::from([(
        document_id,
        sample_document_row_for_preflight(document_id, "provider_alpha_setup.md"),
    )]);
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "Provider Alpha setup".to_string(),
            excerpt: "Install alpha-connector and configure the connector file.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.95),
            source_text: repair_technical_layout_noise(
                r#"
Install the module:
aptitude install alpha-connector

Connector settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
"#,
            ),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "Provider Alpha setup".to_string(),
            excerpt: "Connector parameter table.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.9),
            source_text: repair_technical_layout_noise(
                "Sheet: Connector settings | Row 1 | Name: endpointUrl | Type: string | Description: Service endpoint",
            ),
        },
    ];
    let mut query_ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["Provider Alpha"],
        ["package", "configuration_file", "parameter"],
    );
    let evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };
    let initial_answer = build_canonical_preflight_answer(
        "How do I configure Provider Alpha?",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &document_index,
        None,
        &evidence,
        &chunks,
    )
    .expect("initial setup preflight answer");

    assert!(initial_answer.contains("`alpha-connector`"), "{initial_answer}");
    query_ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "those settings".to_string(),
    }];

    let follow_up_answer = build_canonical_preflight_answer(
        "Explain those settings",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &document_index,
        None,
        &evidence,
        &chunks,
    );

    assert!(follow_up_answer.is_none());
}

#[test]
fn canonical_preflight_answer_preserves_direct_table_follow_up_answers() {
    let document_id = Uuid::now_v7();
    let document_index = HashMap::from([(
        document_id,
        sample_document_row_for_preflight(document_id, "provider_alpha_setup.md"),
    )]);
    let mut query_ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["Provider Alpha"],
        ["table_row"],
    );
    query_ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "that table".to_string(),
    }];
    let evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };
    let answer = build_canonical_preflight_answer(
        "Show the same table rows",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &document_index,
        Some("direct table answer".to_string()),
        &evidence,
        &[],
    )
    .expect("direct table follow-up answer");

    assert_eq!(answer, "direct table answer");
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
fn select_technical_literal_chunks_keeps_setup_block_for_configuration_answers() {
    let question = "Configure Provider Alpha: give the package, module conf file, and merchant secret id parameters.";
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let query_ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["merchant", "secret", "id"],
        ["package", "configuration_file", "parameter"],
    );
    let chunk = |index: i32, text: &str, score: f32| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id,
        chunk_index: index,
        chunk_kind: None,
        document_id,
        document_label: "provider-alpha-setup.md".to_string(),
        excerpt: text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: repair_technical_layout_noise(text),
    };
    let chunks = vec![
        chunk(
            1,
            "Install module:\naptitude install alpha-connector\n\nConfigure module:\ndpkg-reconfigure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf in section [Main].",
            0.80,
        ),
        chunk(2, "| merchantId | string | Partner identifier |", 0.99),
        chunk(3, "| secretKey | string | Authorization secret |", 0.98),
        chunk(4, "| staticQrId | string | Static QR identifier |", 0.97),
        chunk(5, "| staticQrPayload | string | Static QR payload |", 0.96),
        chunk(6, "| timeout | integer | Request timeout |", 0.95),
        chunk(7, "| fillPaymentDetails | boolean | Fill payment details |", 0.94),
    ];

    let selected = select_technical_literal_chunks(
        question,
        &query_ir,
        &chunks,
        TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        1,
        &technical_literal_focus_keywords(question, Some(&query_ir)),
        &[],
        false,
    );

    assert!(selected.iter().any(|chunk| chunk.source_text.contains("aptitude install")));
    assert!(
        selected
            .iter()
            .any(|chunk| chunk.source_text.contains("/opt/alpha/modules/connector/connector.conf"))
    );
    assert!(selected.iter().any(|chunk| chunk.source_text.contains("fillPaymentDetails")));
}

#[test]
fn select_technical_literal_chunks_prefers_matching_wsdl_document_for_single_source_question() {
    let question = "Which WSDL does the inventory soap api use?";
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

#[test]
fn select_technical_literal_chunks_prefers_graph_supported_single_source_document() {
    let question = "How do I configure AcmePay: file, sections, parameters, and an ini example?";
    let generic_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let mut ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["AcmePay"],
        ["configuration", "parameter"],
    );
    ir.target_entities
        .push(EntityMention { label: "AcmePay".to_string(), role: EntityRole::Subject });
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
                document_label: "general_configuration_guide.md".to_string(),
                excerpt: "General configuration guide.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: "General configuration file sections parameters examples configuration file sections parameters examples.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 7,
                chunk_kind: None,
                document_id: target_document_id,
                document_label: "acmepay_processing_guide.md".to_string(),
                excerpt: "AcmePay processor settings.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
                score: Some(0.72),
                source_text:
                    "AcmePay configuration file /opt/acme/acmepay.ini section [Main] paymentEnabled = true section [Receipt] printSlip = false."
                        .to_string(),
            },
        ],
        TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        8,
        &technical_literal_focus_keywords(question, Some(&ir)),
        &[target_document_id],
        false,
    );

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == target_document_id));
    assert!(selected.iter().any(|chunk| chunk.source_text.contains("/opt/acme/acmepay.ini")));
}

#[test]
fn setup_module_and_configuration_targets_use_single_document_literal_scope() {
    let ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["configuration_file", "software_module", "package"],
    );

    assert!(question_prefers_single_exact_literal_scope(
        "How do I configure Provider Alpha package and configuration file?",
        &ir
    ));
}

#[test]
fn follow_up_setup_scope_stays_broad_when_current_question_has_no_exact_literal() {
    let mut ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["alphaMerchantId", "alphaSecret", "staticAlphaPayload"],
        ["parameter"],
    );
    ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "these settings".to_string(),
    }];
    let question = "scope: previous grounded answer named several config keys\nquestion: explain how to configure all these settings";

    assert!(!question_prefers_single_exact_literal_scope(question, &ir));
}

#[test]
fn follow_up_setup_scope_can_focus_when_current_question_names_exact_literal() {
    let mut ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["staticAlphaPayload"],
        ["parameter"],
    );
    ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "that setting".to_string(),
    }];
    let question = "scope: previous grounded answer named several config keys\nquestion: explain staticAlphaPayload";

    assert!(question_prefers_single_exact_literal_scope(question, &ir));
}

#[test]
fn setup_preflight_keeps_deep_parameter_rows_from_selected_document() {
    let question = "How do I configure Provider Alpha: package, config file, and key parameters?";
    let target_document_id = Uuid::now_v7();
    let noisy_document_id = Uuid::now_v7();
    let mut literal_chunks = Vec::new();
    for index in 0..26 {
        let source_text = match index {
            0 => "Provider Alpha setup guide".to_string(),
            1 => {
                "Install package alpha-connector. Configure /opt/alpha/connector/connector.conf."
                    .to_string()
            }
            21 => {
                "| sendDetails | boolean | true false | Send detailed payment payload | Default true |"
                    .to_string()
            }
            _ => format!("Provider Alpha configuration continuation row {index}"),
        };
        literal_chunks.push(RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: None,
            document_id: target_document_id,
            document_label: "Provider Alpha setup guide".to_string(),
            excerpt: source_text.clone(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.80 - (index as f32 / 1_000.0)),
            source_text,
        });
    }
    for index in 0..32 {
        literal_chunks.push(RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: None,
            document_id: noisy_document_id,
            document_label: format!("Provider Alpha screenshot {index}.png"),
            excerpt: "Provider Alpha checkout screen".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: "Provider Alpha checkout screen".to_string(),
        });
    }
    let ir = query_ir_with_act_scope_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["configuration_file", "package", "config_key"],
    );
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };

    let preflight_chunks =
        build_preflight_answer_chunks(question, &ir, &profile, &[], &literal_chunks);

    assert!(!preflight_chunks.is_empty());
    assert!(
        preflight_chunks.iter().all(|chunk| chunk.document_id == target_document_id),
        "{preflight_chunks:?}"
    );
    assert!(preflight_chunks.iter().any(|chunk| chunk.source_text.contains("sendDetails")));
}

#[test]
fn select_technical_literal_chunks_does_not_let_label_overlap_beat_stronger_chunks() {
    let question = "How do I configure AcmePay: file, sections, parameters, and an ini example?";
    let strong_document_id = Uuid::now_v7();
    let weak_label_document_id = Uuid::now_v7();
    let mut ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["AcmePay"],
        ["configuration", "parameter"],
    );
    ir.target_entities
        .push(EntityMention { label: "AcmePay".to_string(), role: EntityRole::Subject });
    let selected = select_technical_literal_chunks(
        question,
        &ir,
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: strong_document_id,
                document_label: "payment_processing_configuration.md".to_string(),
                excerpt: "Configuration file /opt/acme/payments.ini uses [Main].".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.94),
                source_text:
                    "Configure file /opt/acme/payments.ini sections [Main] and [Receipt]. Parameters paymentEnabled = true and printSlip = false. Example ini block included."
                        .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: weak_label_document_id,
                document_label: "acmepay_overview.md".to_string(),
                excerpt: "AcmePay overview.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: "AcmePay overview and commercial background.".to_string(),
            },
        ],
        TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        8,
        &technical_literal_focus_keywords(question, Some(&ir)),
        &[],
        false,
    );

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == strong_document_id));
}

#[test]
fn build_preflight_canonical_evidence_scopes_exact_literal_questions_to_literal_documents() {
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let rewards_revision_id = Uuid::now_v7();
    let inventory_revision_id = Uuid::now_v7();
    let filtered = build_preflight_canonical_evidence(
        "What is the pageNumber parameter called in the pagination API?",
        &query_ir_with_literals_and_target_types(["pageNumber"], ["parameter"]),
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: vec![
                sample_chunk_row(Uuid::now_v7(), rewards_document_id, rewards_revision_id),
                sample_chunk_row(Uuid::now_v7(), inventory_document_id, inventory_revision_id),
            ],
            structured_blocks: vec![
                sample_structured_block_row(
                    Uuid::now_v7(),
                    rewards_document_id,
                    rewards_revision_id,
                ),
                sample_structured_block_row(
                    Uuid::now_v7(),
                    inventory_document_id,
                    inventory_revision_id,
                ),
            ],
            technical_facts: vec![
                sample_technical_fact_row(Uuid::now_v7(), rewards_document_id, rewards_revision_id),
                sample_technical_fact_row(
                    Uuid::now_v7(),
                    inventory_document_id,
                    inventory_revision_id,
                ),
            ],
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: rewards_document_id,
            document_label: "rewards_accounts_api_contract.md".to_string(),
            excerpt: "| `pageNumber` | 1-based page number |".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: "| `pageNumber` | 1-based page number |".to_string(),
        }],
    );

    assert_eq!(
        filtered.chunk_rows.iter().map(|row| row.document_id).collect::<HashSet<_>>(),
        HashSet::from([rewards_document_id])
    );
    assert_eq!(
        filtered.structured_blocks.iter().map(|block| block.document_id).collect::<HashSet<_>>(),
        HashSet::from([rewards_document_id])
    );
    assert_eq!(
        filtered.technical_facts.iter().map(|fact| fact.document_id).collect::<HashSet<_>>(),
        HashSet::from([rewards_document_id])
    );
}

#[test]
fn build_preflight_canonical_evidence_keeps_corpus_for_follow_up_when_literal_scope_has_no_overlap()
{
    let canonical_document_id = Uuid::now_v7();
    let scoped_document_id = Uuid::now_v7();
    let canonical_revision_id = Uuid::now_v7();
    let scoped_revision_id = Uuid::now_v7();
    let question = "How should Provider Alpha runtime settings be configured?";
    let mut ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["Provider Alpha", "runtime settings"],
        ["configuration", "parameter"],
    );
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let canonical_evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: vec![sample_chunk_row(
            Uuid::now_v7(),
            canonical_document_id,
            canonical_revision_id,
        )],
        structured_blocks: vec![KnowledgeStructuredBlockRow {
            normalized_text: "Provider Alpha runtime settings use alpha.ini [Main].".to_string(),
            text: "Provider Alpha runtime settings use alpha.ini [Main].".to_string(),
            ..sample_structured_block_row(
                Uuid::now_v7(),
                canonical_document_id,
                canonical_revision_id,
            )
        }],
        technical_facts: vec![KnowledgeTechnicalFactRow {
            fact_kind: "configuration_file".to_string(),
            canonical_value_text: "alpha.ini".to_string(),
            canonical_value_exact: "alpha.ini".to_string(),
            canonical_value_json: json!("alpha.ini"),
            display_value: "alpha.ini".to_string(),
            ..sample_technical_fact_row(
                Uuid::now_v7(),
                canonical_document_id,
                canonical_revision_id,
            )
        }],
    };
    let technical_literal_chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: scoped_revision_id,
        chunk_index: 0,
        chunk_kind: None,
        document_id: scoped_document_id,
        document_label: "provider_alpha_release_notes.md".to_string(),
        excerpt: "Provider Alpha runtime settings are mentioned in release notes.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.99),
        source_text: "Provider Alpha runtime settings are mentioned in release notes.".to_string(),
    }];

    let scoped_documents =
        preflight_exact_literal_document_scope(question, &ir, &profile, &technical_literal_chunks)
            .expect("literal scope");
    assert_eq!(scoped_documents, HashSet::from([scoped_document_id]));

    let non_follow_up_evidence = build_preflight_canonical_evidence(
        question,
        &ir,
        &profile,
        &canonical_evidence,
        &technical_literal_chunks,
    );
    assert!(non_follow_up_evidence.chunk_rows.is_empty());
    assert!(non_follow_up_evidence.structured_blocks.is_empty());
    assert!(non_follow_up_evidence.technical_facts.is_empty());

    let effective_question = format!(
        "scope: Provider Alpha setup was selected in the previous turn\nquestion: {question}"
    );
    let structured_non_follow_up_evidence = build_preflight_canonical_evidence(
        &effective_question,
        &ir,
        &profile,
        &canonical_evidence,
        &technical_literal_chunks,
    );
    assert!(structured_non_follow_up_evidence.chunk_rows.is_empty());
    assert!(structured_non_follow_up_evidence.structured_blocks.is_empty());
    assert!(structured_non_follow_up_evidence.technical_facts.is_empty());

    ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "those settings".to_string(),
    }];
    let preflight_evidence = build_preflight_canonical_evidence(
        question,
        &ir,
        &profile,
        &canonical_evidence,
        &technical_literal_chunks,
    );

    assert_eq!(
        preflight_evidence.chunk_rows.iter().map(|row| row.document_id).collect::<HashSet<_>>(),
        HashSet::from([canonical_document_id])
    );
    assert_eq!(
        preflight_evidence
            .structured_blocks
            .iter()
            .map(|block| block.document_id)
            .collect::<HashSet<_>>(),
        HashSet::from([canonical_document_id])
    );
    assert_eq!(
        preflight_evidence
            .technical_facts
            .iter()
            .map(|fact| fact.document_id)
            .collect::<HashSet<_>>(),
        HashSet::from([canonical_document_id])
    );
}

#[test]
fn focused_document_preflight_ignores_spurious_exact_literal_document_scope() {
    let pdf_document_id = Uuid::now_v7();
    let docx_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["secondary_heading", "config_key"],
    );
    ir.literal_constraints.push(LiteralSpan {
        text: "upload_smoke_fixture.docx".to_string(),
        kind: LiteralKind::Path,
    });
    ir.literal_constraints.push(LiteralSpan {
        text: "runtime PDF upload check".to_string(),
        kind: LiteralKind::Identifier,
    });
    let pdf_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: pdf_document_id,
        document_label: "runtime_upload_check.pdf".to_string(),
        excerpt: "Runtime PDF upload check".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.7),
        source_text: "Runtime PDF upload check\n\nQuarterly graph report".to_string(),
    };
    let docx_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: docx_document_id,
        document_label: "runtime_upload_check.docx".to_string(),
        excerpt: "Runtime DOCX upload check".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.95),
        source_text: "Runtime DOCX upload check\n\nCanonical pipeline validation".to_string(),
    };

    assert!(
        preflight_exact_literal_document_scope(
            "What report name appears in the runtime PDF upload check?",
            &ir,
            &profile,
            std::slice::from_ref(&docx_chunk),
        )
        .is_none()
    );

    let chunks = build_preflight_answer_chunks(
        "What report name appears in the runtime PDF upload check?",
        &ir,
        &profile,
        std::slice::from_ref(&pdf_chunk),
        std::slice::from_ref(&docx_chunk),
    );
    let answer = build_canonical_preflight_answer(
        "What report name appears in the runtime PDF upload check?",
        &ir,
        &profile,
        &HashMap::from([
            (
                pdf_document_id,
                sample_document_row_for_preflight(pdf_document_id, "runtime_upload_check.pdf"),
            ),
            (
                docx_document_id,
                sample_document_row_for_preflight(docx_document_id, "runtime_upload_check.docx"),
            ),
        ]),
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &chunks,
    )
    .expect("focused document preflight answer");

    assert_eq!(answer, "Quarterly graph report");
}

#[test]
fn preflight_context_keeps_graph_evidence_alongside_literal_document_scope() {
    let scoped_document_id = Uuid::now_v7();
    let rare_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let question = "Which port does the rare calibration service use?";
    let ir = query_ir_with_literals_and_target_types(["calibration service"], ["port"]);
    let literal_chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: scoped_document_id,
        document_label: "release_notes.md".to_string(),
        excerpt: "Calibration service was mentioned in a release note.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.97),
        source_text: "Calibration service was mentioned in a release note.".to_string(),
    }];
    let scoped_documents =
        preflight_exact_literal_document_scope(question, &ir, &profile, &literal_chunks)
            .expect("exact-literal preflight scope");
    assert_eq!(scoped_documents, HashSet::from([scoped_document_id]));
    let graph_lines = build_preflight_graph_evidence_context_lines(&[format!(
        "[graph-evidence target=\"Rare calibration service\" kind=\"configuration\"]\n\
         Source document: rare_calibration_setup.md\n\
         Evidence: Rare calibration service listens on port `3201` from `calibration.ini`.\n\
         document_id={rare_document_id}"
    )]);
    let context = build_canonical_answer_context(
        question,
        &ir,
        None,
        &build_preflight_canonical_evidence(
            question,
            &ir,
            &profile,
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &literal_chunks,
        ),
        &build_preflight_answer_chunks(question, &ir, &profile, &[], &literal_chunks),
        &graph_lines,
    );

    assert!(context.contains("rare_calibration_setup.md"), "{context}");
    assert!(context.contains("`3201`"), "{context}");
    assert!(context.contains("calibration.ini"), "{context}");
}

#[test]
fn canonical_preflight_answer_uses_literal_scoped_evidence_for_parameter_question() {
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let rewards_revision_id = Uuid::now_v7();
    let inventory_revision_id = Uuid::now_v7();
    let document_index = HashMap::from([
        (
            rewards_document_id,
            sample_document_row_for_preflight(
                rewards_document_id,
                "rewards_accounts_api_contract.md",
            ),
        ),
        (
            inventory_document_id,
            sample_document_row_for_preflight(
                inventory_document_id,
                "inventory_soap_api_contract.md",
            ),
        ),
    ]);
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let canonical_evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: vec![
            KnowledgeStructuredBlockRow {
                normalized_text: "| `pageNumber` | 1-based page number |".to_string(),
                text: "| `pageNumber` | 1-based page number |".to_string(),
                ..sample_structured_block_row(
                    Uuid::now_v7(),
                    rewards_document_id,
                    rewards_revision_id,
                )
            },
            KnowledgeStructuredBlockRow {
                normalized_text: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                text: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                ..sample_structured_block_row(
                    Uuid::now_v7(),
                    inventory_document_id,
                    inventory_revision_id,
                )
            },
        ],
        technical_facts: vec![
            KnowledgeTechnicalFactRow {
                fact_kind: "parameter_name".to_string(),
                canonical_value_text: "pageNumber".to_string(),
                canonical_value_exact: "pageNumber".to_string(),
                canonical_value_json: json!({ "value_type": "text", "value": "pageNumber" }),
                display_value: "pageNumber".to_string(),
                ..sample_technical_fact_row(
                    Uuid::now_v7(),
                    rewards_document_id,
                    rewards_revision_id,
                )
            },
            KnowledgeTechnicalFactRow {
                fact_kind: "url".to_string(),
                canonical_value_text: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                canonical_value_exact: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                canonical_value_json: json!(
                    "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                ),
                display_value: "http://demo.local:8080/inventory-api/ws/inventory.wsdl".to_string(),
                ..sample_technical_fact_row(
                    Uuid::now_v7(),
                    inventory_document_id,
                    inventory_revision_id,
                )
            },
        ],
    };
    let technical_literal_chunks = vec![RuntimeMatchedChunk {
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
            "Pagination parameters\n| Parameter | Meaning |\n| `pageNumber` | 1-based page number |",
        ),
    }];
    let ir = query_ir_with_literals_and_target_types(["pageNumber"], ["parameter"]);
    let preflight_chunks = build_preflight_answer_chunks(
        "What is the pageNumber parameter called in the pagination API?",
        &ir,
        &profile,
        &[
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
                score: Some(0.98),
                source_text: repair_technical_layout_noise(
                    "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                ),
            },
            technical_literal_chunks[0].clone(),
        ],
        &technical_literal_chunks,
    );
    let preflight_evidence = build_preflight_canonical_evidence(
        "What is the pageNumber parameter called in the pagination API?",
        &ir,
        &profile,
        &canonical_evidence,
        &technical_literal_chunks,
    );

    let answer = build_canonical_preflight_answer(
        "What is the pageNumber parameter called in the pagination API?",
        &ir,
        &profile,
        &document_index,
        None,
        &preflight_evidence,
        &preflight_chunks,
    )
    .expect("parameter preflight answer");

    assert!(answer.contains("`pageNumber`"), "{answer}");
    assert!(!answer.contains("inventory"), "{answer}");
}

#[test]
fn preflight_exact_literal_scope_prefers_focused_document_for_single_source_question() {
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let question = "Which WSDL does the inventory soap api use?";
    let ir = query_ir_with_literals_and_target_types(["inventory soap api"], ["endpoint", "wsdl"]);
    let technical_literal_chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: rewards_document_id,
            document_label: "rewards_accounts_api_contract.md".to_string(),
            excerpt: "GET /v1/accounts returns rewards accounts.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: repair_technical_layout_noise(
                "Rewards Accounts API Contract\nGET /v1/accounts\nwithCards",
            ),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: inventory_document_id,
            document_label: "inventory_soap_api_contract.md".to_string(),
            excerpt: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.97),
            source_text: repair_technical_layout_noise(
                "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
            ),
        },
    ];

    let preflight_chunks = build_preflight_answer_chunks(
        question,
        &ir,
        &profile,
        &technical_literal_chunks,
        &technical_literal_chunks,
    );
    let preflight_evidence = build_preflight_canonical_evidence(
        question,
        &ir,
        &profile,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![
                KnowledgeStructuredBlockRow {
                    normalized_text: "GET /v1/accounts".to_string(),
                    text: "GET /v1/accounts".to_string(),
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        rewards_document_id,
                        Uuid::now_v7(),
                    )
                },
                KnowledgeStructuredBlockRow {
                    normalized_text:
                        "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                            .to_string(),
                    text: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                        .to_string(),
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        inventory_document_id,
                        Uuid::now_v7(),
                    )
                },
            ],
            technical_facts: Vec::new(),
        },
        &technical_literal_chunks,
    );

    assert_eq!(
        preflight_chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>(),
        HashSet::from([inventory_document_id])
    );
    assert_eq!(
        preflight_evidence
            .structured_blocks
            .iter()
            .map(|block| block.document_id)
            .collect::<HashSet<_>>(),
        HashSet::from([inventory_document_id])
    );
}

#[test]
fn preflight_exact_literal_scope_prefers_document_focus_label_over_noisy_chunk_signal() {
    let focused_document_id = Uuid::now_v7();
    let noisy_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let question = "How do I configure Provider Alpha payment settings?";
    let mut ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["payment settings", "config file", "Main", "Check"],
        ["configuration_file", "config_key"],
    );
    ir.document_focus = Some(DocumentHint { hint: "Provider Alpha in Retail Suite".to_string() });
    ir.target_entities = vec![
        EntityMention { label: "Main".to_string(), role: EntityRole::Subject },
        EntityMention { label: "Check".to_string(), role: EntityRole::Subject },
    ];

    let technical_literal_chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: noisy_document_id,
            document_label: "Configuration editor screenshot.png".to_string(),
            excerpt: "Main Check config file payment settings payment settings.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: "Main Check config file payment settings payment settings.".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: focused_document_id,
            document_label: "Provider Alpha payment guide".to_string(),
            excerpt: "Payment settings are configured in the module configuration.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.72),
            source_text: "Payment settings are configured in the module configuration.".to_string(),
        },
    ];

    assert_eq!(
        preflight_exact_literal_document_scope(question, &ir, &profile, &technical_literal_chunks),
        Some(HashSet::from([focused_document_id]))
    );
}

#[test]
fn preflight_exact_literal_scope_uses_current_question_segment_not_history_scope_noise() {
    let focused_document_id = Uuid::now_v7();
    let noisy_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let question = "scope: literals: `/var/log/alpha.log`, `/var/log/alpha-debug.log`, `device_id`, `updated_at`\nProvider Alpha settings use package `alpha-provider-module`, configuration file `/opt/alpha/alpha.conf`, and parameter defaults.\nquestion: explain all settings";
    let ir = query_ir_with_act_scope_and_target_types(
        QueryAct::Describe,
        QueryScope::SingleDocument,
        ["parameter"],
    );
    let technical_literal_chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: noisy_document_id,
            document_label: "device log export table".to_string(),
            excerpt: "/var/log/alpha.log /var/log/alpha-debug.log device_id updated_at".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: "/var/log/alpha.log /var/log/alpha-debug.log device_id updated_at"
                .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: focused_document_id,
            document_label: "Provider Alpha setup guide".to_string(),
            excerpt: "Provider Alpha settings include alpha_timeout and alpha_endpoint."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.72),
            source_text: "Provider Alpha settings include alpha_timeout and alpha_endpoint."
                .to_string(),
        },
    ];

    assert_eq!(
        preflight_exact_literal_document_scope(question, &ir, &profile, &technical_literal_chunks),
        Some(HashSet::from([focused_document_id]))
    );
}

#[test]
fn preflight_exact_literal_scope_keeps_multi_document_comparison_questions_broad() {
    let checkout_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };

    let scoped_documents = preflight_exact_literal_document_scope(
        "How does rewards REST differ from inventory WSDL?",
        &generic_query_ir(),
        &profile,
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: checkout_document_id,
                document_label: "rewards_accounts_api_contract.md".to_string(),
                excerpt: "REST API over JSON.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: "REST API over JSON.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "SOAP API with WSDL.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.97),
                source_text: "SOAP API with WSDL.".to_string(),
            },
        ],
    )
    .expect("comparison questions should keep document scope");

    assert_eq!(scoped_documents, HashSet::from([checkout_document_id, inventory_document_id]));
}

#[test]
fn preflight_exact_literal_scope_stays_broad_for_error_code_intent() {
    let document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["error_code"],
    );
    let technical_literal_chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "payment-screen.png".to_string(),
        excerpt: "Code Currency Name".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.9),
        source_text: "Code Currency Name".to_string(),
    }];

    assert_eq!(
        preflight_exact_literal_document_scope(
            "Which error codes can the card processor return?",
            &ir,
            &profile,
            &technical_literal_chunks,
        ),
        None
    );
}

#[test]
fn preflight_exact_literal_scope_stays_broad_for_transport_inventory_intent() {
    let document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["port", "protocol", "connection"],
    );
    let technical_literal_chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "checkout-networking.md".to_string(),
        excerpt: "Set serviceUrl=http://checkout.local:8080 for normal operation.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.9),
        source_text: "Set serviceUrl=http://checkout.local:8080 for normal operation.".to_string(),
    }];

    assert_eq!(
        preflight_exact_literal_document_scope(
            "Which ports and network connections are required?",
            &ir,
            &profile,
            &technical_literal_chunks,
        ),
        None
    );
}

#[test]
fn canonical_preflight_answer_handles_english_transport_comparison_without_graphql_noise() {
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let document_index = HashMap::from([
        (
            rewards_document_id,
            sample_document_row_for_preflight(
                rewards_document_id,
                "rewards_accounts_api_contract.md",
            ),
        ),
        (
            inventory_document_id,
            sample_document_row_for_preflight(
                inventory_document_id,
                "inventory_soap_api_contract.md",
            ),
        ),
    ]);
    let question = "How does the REST API for rewards accounts differ from the inventory WSDL transport contract?";
    let answer = build_canonical_preflight_answer(
        question,
        &query_ir_with_act_scope_and_target_types(
            QueryAct::Compare,
            QueryScope::MultiDocument,
            ["protocol"],
        ),
        &QueryIntentProfile::default(),
        &document_index,
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: rewards_document_id,
                document_label: "rewards_accounts_api_contract.md".to_string(),
                excerpt: "REST JSON over HTTP".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text:
                    "The rewards accounts surface is a REST API that returns JSON over HTTP."
                        .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: inventory_document_id,
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "SOAP WSDL over HTTP".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.97),
                source_text:
                    "The inventory integration surface is SOAP over HTTP and described by WSDL."
                        .to_string(),
            },
        ],
    )
    .expect("comparison preflight answer");

    let lowered = answer.to_lowercase();
    assert!(lowered.contains("rewards accounts"), "{answer}");
    assert!(lowered.contains("inventory"), "{answer}");
    assert!(lowered.contains("rest"), "{answer}");
    assert!(lowered.contains("wsdl"), "{answer}");
    assert!(!lowered.contains("graphql"), "{answer}");
}
