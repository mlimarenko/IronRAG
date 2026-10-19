use super::*;

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
            "Install module:\nsample-install alpha-connector\n\nConfigure module:\nsample-configure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf in section [Main].",
            0.80,
        ),
        chunk(2, "| merchantId | string | Partner identifier |", 0.99),
        chunk(3, "| secretKey | string | Authorization secret |", 0.98),
        chunk(4, "| staticQrId | string | Static QR identifier |", 0.97),
        chunk(5, "| staticQrPayload | string | Static QR payload |", 0.96),
        chunk(6, "| timeout | integer | Request timeout |", 0.95),
        chunk(7, "| fillPaymentDetails | boolean | Fill payment details |", 0.94),
    ];

    let selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &query_ir,
        chunks: &chunks,
        technical_literal_intent: TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        top_k: 1,
        literal_focus_keywords: &technical_literal_focus_keywords(question, Some(&query_ir)),
        preferred_document_ids: &[],
        pagination_requested: false,
    });

    assert!(selected.iter().any(|chunk| chunk.source_text.contains("sample-install")));
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
        technical_literal_intent: TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        top_k: 8,
        literal_focus_keywords: &technical_literal_focus_keywords(question, Some(&ir)),
        preferred_document_ids: &[target_document_id],
        pagination_requested: false,
    });

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
fn retrieve_value_config_key_with_focus_uses_setup_literal_context() {
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["config_key"],
    );
    ir.target_entities
        .push(EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject });

    assert!(query_ir_requests_setup_literal_context(&ir));
}

#[test]
fn low_confidence_parameter_surface_requests_preflight_bridge_without_raw_question_signal() {
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::Describe,
        QueryScope::SingleDocument,
        [],
    );
    ir.confidence = 0.25;
    let source_context = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 12,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "Provider Alpha setup guide".to_string(),
        excerpt: "QX visibleMode = true".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(0.42),
        source_text: "QX visibleMode = true".to_string(),
    };

    assert!(query_ir_requests_low_confidence_setup_preflight(&ir, &[source_context]));
}

#[test]
fn low_confidence_setup_context_requests_preflight_bridge_without_short_focus_token() {
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::Describe,
        QueryScope::SingleDocument,
        [],
    );
    ir.confidence = 0.25;
    let source_context = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 2,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "Provider Alpha setup guide".to_string(),
        excerpt: "Configure /opt/alpha/alpha.conf section [Main].".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(0.42),
        source_text: "Configure /opt/alpha/alpha.conf section [Main]. alphaMode = true".to_string(),
    };

    assert!(query_ir_requests_low_confidence_setup_preflight(&ir, &[source_context]));
}

#[test]
fn low_confidence_structural_setup_context_requests_preflight_bridge() {
    let mut ir =
        query_ir_with_act_scope_and_target_types(QueryAct::Describe, QueryScope::MultiDocument, []);
    ir.confidence = 0.25;
    ir.target_entities
        .push(EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject });
    let source_context = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 2,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "Provider Alpha setup guide".to_string(),
        excerpt: "Configure /opt/alpha/alpha.conf section [Main].".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(0.42),
        source_text: "Configure /opt/alpha/alpha.conf section [Main]. alphaMode = true".to_string(),
    };

    assert!(query_ir_requests_low_confidence_setup_preflight(&ir, &[source_context]));
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
    let selected = select_technical_literal_chunks(TechnicalLiteralSelection {
        question,
        query_ir: &ir,
        chunks: &[
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
        technical_literal_intent: TechnicalLiteralIntent {
            wants_paths: true,
            wants_parameters: true,
            ..TechnicalLiteralIntent::default()
        },
        top_k: 8,
        literal_focus_keywords: &technical_literal_focus_keywords(question, Some(&ir)),
        preferred_document_ids: &[],
        pagination_requested: false,
    });

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == strong_document_id));
}
