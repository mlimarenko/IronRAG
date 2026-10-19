use super::*;

#[test]
fn build_deterministic_grounded_answer_prefers_exact_wsdl_document_over_foreign_focus_noise() {
    let inventory_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let inventory_revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which WSDL does the inventory SOAP API use?",
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["inventory soap api"],
            ["endpoint", "wsdl"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "url".to_string(),
                normalized_value_text: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                normalized_value_exact: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                normalized_value_json: json!(
                    "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                ),
                display_value: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                ..sample_technical_fact_row(Uuid::now_v7(), inventory_document_id, inventory_revision_id)
            }],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: rewards_document_id,
                document_label: "rewards_accounts_api_contract.md".to_string(),
                excerpt: "Compared with the inventory SOAP surface, rewards accounts use REST JSON."
                    .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "Rewards Accounts API Contract\nCompared with the inventory SOAP surface, rewards accounts use REST JSON.",
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
                score: Some(0.71),
                source_text: repair_technical_layout_noise(
                    "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(answer.contains("inventory"));
    assert!(answer.contains("`http://demo.local:8080/inventory-api/ws/inventory.wsdl`"));
}

#[test]
fn build_deterministic_grounded_answer_uses_parameter_meaning_from_structured_block() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let block_id = Uuid::now_v7();
    let chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "rewards_accounts_api_contract.md".to_string(),
        excerpt: "| `pageNumber` | 1-based page number |".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.96),
        source_text: repair_technical_layout_noise(
            "Pagination parameters\n| Parameter | Meaning |\n| `pageNumber` | 1-based page number |",
        ),
    };
    let answer = build_deterministic_grounded_answer(
        "What is the pageNumber parameter called in the pagination API?",
        &query_ir_with_literals_and_target_types(["pageNumber"], ["parameter"]),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text: "| `pageNumber` | 1-based page number |".to_string(),
                text: "| `pageNumber` | 1-based page number |".to_string(),
                ..sample_structured_block_row(block_id, document_id, revision_id)
            }],
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "parameter_name".to_string(),
                normalized_value_text: "pageNumber".to_string(),
                normalized_value_exact: "pageNumber".to_string(),
                normalized_value_json: json!({ "value_type": "text", "value": "pageNumber" }),
                display_value: "pageNumber".to_string(),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[chunk],
    )
    .unwrap_or_default();

    assert!(answer.contains("`pageNumber`"), "{answer}");
    assert!(answer.contains("1-based page number"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_preserves_setup_config_sections() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let answer = build_deterministic_grounded_answer(
        "How do I configure Subject Alpha: package, primary file, and parameters?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::ConfigureHow,
            QueryScope::SingleDocument,
            ["package", "configuration_file", "parameter"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text:
                    "[CFG]\nRow 1 | Name: endpointUrl | Type: string | Default: http://example.invalid\nRow 2 | Name: accountId | Type: string | Default: empty\nRow 3 | Name: includeDetails | Type: boolean | Values: true false | Default: true\nRow 4 | Name: printSummary | Type: boolean | Values: true false | Default: false\n| captureSummary | boolean | true false | false |"
                        .to_string(),
                text:
                    "[CFG]\nRow 1 | Name: endpointUrl | Type: string | Default: http://example.invalid\nRow 2 | Name: accountId | Type: string | Default: empty\nRow 3 | Name: includeDetails | Type: boolean | Values: true false | Default: true\nRow 4 | Name: printSummary | Type: boolean | Values: true false | Default: false\n| captureSummary | boolean | true false | false |"
                        .to_string(),
                ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
            }],
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "Subject Alpha setup".to_string(),
            excerpt: "Install module-alpha and configure CFG.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.96),
            source_text: repair_technical_layout_noise(
                "sample-install module-alpha\nPrimary file: /opt/subject-alpha/alpha.conf\n[CFG]\nRow 1 | Name: endpointUrl | Type: string | Default: http://example.invalid\nRow 2 | Name: accountId | Type: string | Default: empty\nRow 3 | Name: includeDetails | Type: boolean | Values: true false | Default: true\nRow 4 | Name: printSummary | Type: boolean | Values: true false | Default: false\n| captureSummary | boolean | true false | false |\n[UI.ScanPanel.qrCode]\nvisible = true",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(answer.contains("`module-alpha`"), "{answer}");
    assert!(answer.contains("`/opt/subject-alpha/alpha.conf`"), "{answer}");
    assert!(answer.contains("`[CFG]`"), "{answer}");
    assert!(answer.contains("`endpointUrl`"), "{answer}");
    assert!(answer.contains("`accountId`"), "{answer}");
    assert!(answer.contains("`includeDetails = true`"), "{answer}");
    assert!(answer.contains("`printSummary = false`"), "{answer}");
    assert!(answer.contains("`captureSummary = false`"), "{answer}");
    assert!(answer.contains("`[UI.ScanPanel.qrCode]`"), "{answer}");
    assert!(answer.contains("`visible = true`"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_preserves_retrieve_value_config_key_sections() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["config_key"],
    );
    query_ir
        .target_entities
        .push(EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject });
    let answer = build_deterministic_grounded_answer(
        "Where is Provider Alpha display enabled: section, key, and value?",
        &query_ir,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text:
                    "/opt/provider-alpha/ui.ini\n[UI.ScanPanel.qrCode]\nvisible = true"
                        .to_string(),
                text: "/opt/provider-alpha/ui.ini\n[UI.ScanPanel.qrCode]\nvisible = true"
                    .to_string(),
                ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
            }],
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 9,
            chunk_kind: None,
            document_id,
            document_label: "Provider Alpha setup".to_string(),
            excerpt: "Provider Alpha display settings.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.91),
            source_text: repair_technical_layout_noise(
                "Provider Alpha display settings are stored in /opt/provider-alpha/ui.ini.\n[UI.ScanPanel.qrCode]\nvisible = true",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(answer.contains("`/opt/provider-alpha/ui.ini`"), "{answer}");
    assert!(answer.contains("`[UI.ScanPanel.qrCode]`"), "{answer}");
    assert!(answer.contains("`visible = true`"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_extracts_error_code_assignment_mapping() {
    let document_id = Uuid::now_v7();
    let answer = build_deterministic_grounded_answer(
        "Which error codes can the card processor return?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["error_code"],
        ),
        &AuthoritativeAnswerEvidence {
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
            document_id,
            document_label: "payment_terminal_config.md".to_string(),
            excerpt: "customerVisibleErrorCodes = 101, 202".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
            score: Some(100_000.0),
            source_text: repair_technical_layout_noise(
                "[Terminal]\ncustomerVisibleErrorCodes = 101, 202\n[ErrorMessages]\n101 = Card unreadable\n202 = Cancelled by customer",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(answer.starts_with("`customerVisibleErrorCodes`"), "{answer}");
    assert!(answer.contains("`customerVisibleErrorCodes`"), "{answer}");
    assert!(answer.contains("`101`"), "{answer}");
    assert!(answer.contains("Card unreadable"), "{answer}");
    assert!(answer.contains("`202`"), "{answer}");
    assert!(answer.contains("Cancelled by customer"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_extracts_transport_config_assignments() {
    let document_id = Uuid::now_v7();
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["port", "connection"],
    );
    // Assignment extraction is eligible only when its source carries the
    // typed question focus; target kinds alone no longer admit a winner.
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "checkout service".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    let answer = build_deterministic_grounded_answer(
        "Which ports and connections does the checkout service require?",
        &query_ir,
        &AuthoritativeAnswerEvidence {
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
            document_id,
            document_label: "checkout_service_config.md".to_string(),
            excerpt: "Example connection settings.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
            score: Some(100_000.0),
            source_text: repair_technical_layout_noise(
                "Example settings [Connector] ;serviceUrl = http://localhost:8080 ;requestTimeout = 30 ;ignoreConnectionErrors = false",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(answer.starts_with("`checkout_service_config.md`"), "{answer}");
    assert!(answer.contains("`serviceUrl` = `http://localhost:8080`"), "{answer}");
    assert!(answer.contains("`requestTimeout` = `30`"), "{answer}");
    assert!(answer.contains("`ignoreConnectionErrors` = `false`"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_finds_parameter_with_question_mark_despite_foreign_noise() {
    let document_id = Uuid::now_v7();
    let foreign_document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let block_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Does a withCards parameter exist?",
        &query_ir_with_literals_and_target_types(["withCards"], ["parameter"]),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text: "| `withCards` | include linked card records in the response |"
                    .to_string(),
                text: "| `withCards` | include linked card records in the response |"
                    .to_string(),
                ..sample_structured_block_row(block_id, document_id, revision_id)
            }],
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "parameter_name".to_string(),
                normalized_value_text: "withCards".to_string(),
                normalized_value_exact: "withCards".to_string(),
                normalized_value_json: json!({ "value_type": "text", "value": "withCards" }),
                display_value: "withCards".to_string(),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: foreign_document_id,
                document_label: "foreign_transport_contract.md".to_string(),
                excerpt: "Foreign transport uses WSDL.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "Foreign Transport Contract\nWSDL URL: http://example.invalid:8080/foreign-api/ws/service.wsdl",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "subject_records_api_contract.md".to_string(),
                excerpt: "withCards includes linked records.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.71),
                source_text: repair_technical_layout_noise(
                    "| `withCards` | include linked card records in the response |",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(answer.contains("Parameter `withCards`"), "{answer}");
    assert!(answer.contains("`withCards`"), "{answer}");
    assert!(answer.contains("include linked card records in the response"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_confirms_parameter_existence() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let block_id = Uuid::now_v7();
    let answer = build_deterministic_grounded_answer(
        "Does a withCards parameter exist?",
        &query_ir_with_literals_and_target_types(["withCards"], ["parameter"]),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text: "| `withCards` | include linked card records in the response |"
                    .to_string(),
                text: "| `withCards` | include linked card records in the response |".to_string(),
                ..sample_structured_block_row(block_id, document_id, revision_id)
            }],
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "parameter_name".to_string(),
                normalized_value_text: "withCards".to_string(),
                normalized_value_exact: "withCards".to_string(),
                normalized_value_json: json!({ "value_type": "text", "value": "withCards" }),
                display_value: "withCards".to_string(),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "rewards_accounts_api_contract.md".to_string(),
            excerpt: "withCards includes linked card records.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.95),
            source_text: repair_technical_layout_noise(
                "| `withCards` | include linked card records in the response |",
            ),
        }],
    )
    .unwrap_or_default();

    assert!(answer.contains("Parameter `withCards`"), "{answer}");
    assert!(answer.contains("`withCards`"), "{answer}");
    assert!(answer.contains("include linked card records in the response"), "{answer}");
}
