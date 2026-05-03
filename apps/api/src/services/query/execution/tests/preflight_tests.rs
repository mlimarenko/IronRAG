use super::*;

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
        &fallback_query_ir(),
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
        &query_ir_with_literals_and_target_types(["current info", "checkout server"], ["endpoint"]),
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

    let merged = build_preflight_answer_chunks(
        "Which endpoint returns the current checkout server info?",
        &query_ir_with_literals_and_target_types(
            ["current information", "checkout server"],
            ["endpoint"],
        ),
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
        &query_ir_with_literals_and_target_types(
            ["current information", "checkout server"],
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
        &merged,
    )
    .expect("single endpoint preflight answer from merged candidates");

    assert_eq!(answer, "The endpoint is `GET /system/info`.");
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
        question_mentions_pagination(question),
    );

    assert_eq!(selected.len(), 1);
    assert!(selected.iter().all(|chunk| chunk.document_id == rewards_document_id));
    assert!(selected.iter().all(|chunk| chunk.source_text.contains("pageNumber")));
    assert!(!selected.iter().any(|chunk| chunk.document_id == inventory_document_id));
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
        question_mentions_pagination(question),
    );

    assert!(!selected.is_empty());
    assert!(selected.iter().all(|chunk| chunk.document_id == inventory_document_id));
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
fn preflight_exact_literal_scope_keeps_multi_document_comparison_questions_broad() {
    let checkout_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };

    let scoped_documents = preflight_exact_literal_document_scope(
        "How does rewards REST differ from inventory WSDL?",
        &fallback_query_ir(),
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
            ["transport", "protocol"],
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
