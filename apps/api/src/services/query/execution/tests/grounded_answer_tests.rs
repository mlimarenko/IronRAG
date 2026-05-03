use super::*;

#[test]
fn build_port_answer_skips_override_when_focused_document_has_no_grounded_port() {
    let control_document_id = Uuid::now_v7();
    let telegram_document_id = Uuid::now_v7();

    let answer = build_port_answer(
        "What port does the Acme Control Center use?",
        &query_ir_with_literals_and_target_types(["Acme Control Center"], ["port"]),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: control_document_id,
                document_label: "Acme Control Center - Example".to_string(),
                excerpt:
                    "Acme Control Center is configuration management software for managed objects."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text: repair_technical_layout_noise(
                    "Acme Control Center\nDescription\nAcme Control Center is configuration management software for managed objects.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: telegram_document_id,
                document_label: "Acme Telegram Bot - Example".to_string(),
                excerpt: "Integration uses localhost:2026.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.91),
                source_text: repair_technical_layout_noise(
                    "Acme Telegram Bot\nSettings\nport: 2026\nlocalhost:2026",
                ),
            },
        ],
    );

    assert!(answer.is_none());
}

#[test]
fn build_port_answer_skips_port_plus_protocol_questions() {
    let rewards_document_id = Uuid::now_v7();
    let loyalty_document_id = Uuid::now_v7();

    let answer = build_port_answer(
        "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
        &query_ir_with_literals_and_target_types(
            ["Rewards Accounts REST API", "Customer Profile API"],
            ["port", "protocol"],
        ),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: rewards_document_id,
                document_label: "rewards_accounts_rest_reference.md".to_string(),
                excerpt: "Default port: 8081".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "Rewards Accounts REST API Reference\nDefault port: 8081\nProtocol: REST over HTTP",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: loyalty_document_id,
                document_label: "customer_profile_soap_reference.md".to_string(),
                excerpt: "Protocol: SOAP over HTTP".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.98),
                source_text: repair_technical_layout_noise(
                    "Customer Profile SOAP API Reference\nProtocol: SOAP over HTTP",
                ),
            },
        ],
    );

    assert!(answer.is_none());
}

#[test]
fn build_port_and_protocol_answer_handles_english_multi_document_question() {
    let rewards_document_id = Uuid::now_v7();
    let loyalty_document_id = Uuid::now_v7();

    let answer = build_port_and_protocol_answer(
            "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
            &query_ir_with_scope_literals_and_target_types(
                QueryScope::MultiDocument,
                ["Rewards Accounts REST API", "Customer Profile API"],
                ["port", "protocol"],
            ),
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: rewards_document_id,
                    document_label: "rewards_accounts_rest_reference.md".to_string(),
                    excerpt: "Default port: 8081".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise(
                        "Rewards Accounts REST API Reference\nDefault port: 8081\nBase REST URL: http://demo.local:8081/rewards-api/rest",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: loyalty_document_id,
                    document_label: "customer_profile_soap_reference.md".to_string(),
                    excerpt: "Protocol: SOAP over HTTP".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise(
                        "Customer Profile SOAP API Reference\nProtocol: SOAP over HTTP\nWSDL URL: http://demo.local:8080/customer-profile/ws/customer-profile.wsdl",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("8081"), "{answer}");
    assert!(answer.contains("SOAP"), "{answer}");
}

#[test]
fn build_multi_document_endpoint_answer_handles_english_checkout_rewards_question() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();

    let answer = build_multi_document_endpoint_answer_from_chunks(
            "If an agent needs the current Checkout Server status and then the Rewards Accounts list, which two endpoints should it call?",
            &query_ir_with_scope_literals_and_target_types(
                QueryScope::MultiDocument,
                ["current Checkout Server status", "Rewards Accounts list"],
                ["endpoint"],
            ),
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: rewards_document_id,
                    document_label: "rewards_accounts_rest_reference.md".to_string(),
                    excerpt: "List accounts: GET /v1/accounts".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise(
                        "Rewards Accounts REST API Reference\nList accounts: GET /v1/accounts\nList cards: GET /v1/cards",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: checkout_document_id,
                    document_label: "checkout_server_rest_reference.md".to_string(),
                    excerpt: "Health check: GET /health".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.96),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API Reference\nHealth check: GET /health",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: checkout_document_id,
                    document_label: "checkout_server_rest_reference.md".to_string(),
                    excerpt: "Current server information: GET /system/info".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.94),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API Reference\nCurrent server information: GET /system/info\n/system/info returns the current checkout server status and runtime metadata.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("/system/info"), "{answer}");
    assert!(answer.contains("/v1/accounts"), "{answer}");
    assert!(!answer.contains("/health"), "{answer}");
}

#[test]
fn build_single_endpoint_answer_from_chunks_prefers_system_info_over_adjacent_noise() {
    let checkout_document_id = Uuid::now_v7();

    let answer = build_single_endpoint_answer_from_chunks(
        "Which endpoint returns the current information for the checkout server?",
        &query_ir_with_literals_and_target_types(["current information", "checkout server"], ["endpoint"]),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: checkout_document_id,
                document_label: "checkout_server_reference.pdf".to_string(),
                excerpt: "GET /checkout-api/rest/dictionaries/cardChanged returns card change history for checkout server.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.96),
                source_text: repair_technical_layout_noise(
                    "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/cardChanged\nGet card change history for checkout server.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: checkout_document_id,
                document_label: "checkout_server_reference.pdf".to_string(),
                excerpt: "To get the current information for the checkout server, send a GET request to /system/info.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.71),
                source_text: repair_technical_layout_noise(
                    "Public checkout server API.\nhttp://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nGet current checkout server status.",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(answer.contains("`GET /system/info`"), "{answer}");
    assert!(!answer.contains("cardChanged"), "{answer}");
}

#[test]
fn build_single_endpoint_answer_from_chunks_prefers_question_matched_document_over_foreign_noise() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();

    let answer = build_single_endpoint_answer_from_chunks(
        "Which endpoint returns the current information for the checkout server?",
        &query_ir_with_literals_and_target_types(
            ["current information", "checkout server"],
            ["endpoint"],
        ),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: rewards_document_id,
                document_label: "rewards_accounts_api_contract.md".to_string(),
                excerpt: "GET /v1/accounts returns Rewards Service account list.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "Rewards Accounts API Contract\nGET /v1/accounts\nTransport: REST JSON",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: checkout_document_id,
                document_label: "checkout_runtime_contract.md".to_string(),
                excerpt: "GET /system/info returns current information for the checkout server."
                    .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.72),
                source_text: repair_technical_layout_noise(
                    "Checkout Runtime Contract\nGET\n/system/info\ncurrent checkout server system information",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(answer.contains("`GET /system/info`"), "{answer}");
    assert!(!answer.contains("/v1/accounts"), "{answer}");
}

#[test]
fn build_single_endpoint_answer_falls_back_to_full_source_when_focus_excerpt_skips_literal() {
    let document_id = Uuid::now_v7();
    let answer = build_single_endpoint_answer_from_chunks(
        "Which endpoint returns the current information for the checkout server?",
        &query_ir_with_literals_and_target_types(
            ["current information", "checkout server"],
            ["endpoint"],
        ),
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "checkout_runtime_contract.md".to_string(),
            excerpt: "# Checkout Runtime Contract".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: repair_technical_layout_noise(
                "# Checkout Runtime Contract\nThe checkout server exposes runtime metadata.\nMethod: GET\nPath: /system/info",
            ),
        }],
    )
    .expect("single endpoint answer");

    assert_eq!(answer, "The endpoint is `GET /system/info`.");
}

#[test]
fn build_multi_document_endpoint_answer_from_chunks_prefers_current_info_for_cash_document() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let answer = build_multi_document_endpoint_answer_from_chunks(
            "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
            &query_ir_with_scope_literals_and_target_types(
                QueryScope::MultiDocument,
                ["current status checkout server", "account list rewards service"],
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
                    score: Some(0.94),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nGet Rewards Service account list.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "GET /checkout-api/rest/dictionaries/cardChanged returns card change history for checkout server.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(0.96),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/cardChanged\nGet card change history for checkout server.",
                    ),
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
                    score: Some(0.71),
                    source_text: repair_technical_layout_noise(
                        "Public checkout server API.\nhttp://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nGet current Checkout Server status.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("`GET /v1/accounts`"));
    assert!(answer.contains("`GET /system/info`"));
    assert!(!answer.contains("cardChanged"));
}

#[test]
fn build_multi_document_endpoint_answer_from_chunks_handles_live_checkout_server_chunk_layout() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let wsdl_document_id = Uuid::now_v7();
    let answer = build_multi_document_endpoint_answer_from_chunks(
            "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
            &query_ir_with_scope_literals_and_target_types(
                QueryScope::MultiDocument,
                ["current status checkout server", "account list rewards service"],
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
                    score: Some(69858.0),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nGet Rewards Service account list.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Get card change history for checkout server.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(70000.0),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/cardChanged\nGet card change history for checkout server.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Public checkout server API. Checkout server provides a REST interface for external services and applications.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API\nCheckout server provides a REST interface for external services and applications. Requests are made via HTTP and data is transmitted as JSON. Prefix for checkout server REST interface: http://<host>:<port>/checkout-api/rest/<request>\nhttp://demo.local:8080/checkout-api/rest/system/info\nTo get the current checkout server status, send a GET request to /system/info.\nResult fields include version, buildNumber and buildDate.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: None,
                    document_id: wsdl_document_id,
                    document_label: "customer_profile_service_reference.pdf".to_string(),
                    excerpt: "Customer profile service WSDL is available at /customer-profile/ws/.".to_string(),
                    score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Get WSDL at http://demo.local:8080/customer-profile/ws/customer-profile.wsdl. Base prefix /customer-profile/ws/.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("`GET /v1/accounts`"));
    assert!(answer.contains("`GET /system/info`"));
    assert!(!answer.contains("cardChanged"));
    assert!(!answer.contains("/customer-profile/ws/"));
}

#[test]
fn build_deterministic_grounded_answer_uses_exact_wsdl_literal_without_agent_loop() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "inventory_soap_api_contract.md".to_string(),
        excerpt: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.98),
        source_text: repair_technical_layout_noise(
            "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
        ),
    };
    let answer = build_deterministic_grounded_answer(
        "Which WSDL does the inventory SOAP API use?",
        &query_ir_with_literals_and_target_types(["inventory soap api"], ["url", "wsdl"]),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "url".to_string(),
                canonical_value_text: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                canonical_value_exact: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                canonical_value_json: json!(
                    "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                ),
                display_value: "http://demo.local:8080/inventory-api/ws/inventory.wsdl".to_string(),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[chunk],
    )
    .unwrap_or_default();

    assert!(answer.contains("inventory"));
    assert!(answer.contains("`http://demo.local:8080/inventory-api/ws/inventory.wsdl`"));
}

#[test]
fn build_deterministic_grounded_answer_uses_endpoint_fact_without_chunk_parsing() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which endpoint returns the current information for the checkout server?",
        &query_ir_with_literals_and_target_types(
            ["checkout server", "current information"],
            ["endpoint"],
        ),
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
        &[],
    )
    .unwrap_or_default();

    assert_eq!(answer, "The endpoint is `GET /system/info`.");
}

#[test]
fn build_deterministic_grounded_answer_abstains_when_endpoint_candidate_misses_parameter_facet() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What pagination approach is recommended, and what query parameters are used?",
        &query_ir_with_literals_and_target_types(
            ["pagination query parameters"],
            ["endpoint", "parameter"],
        ),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    fact_kind: "endpoint_path".to_string(),
                    canonical_value_text: "/resources/{id}".to_string(),
                    canonical_value_exact: "/resources/{id}".to_string(),
                    canonical_value_json: json!("/resources/{id}"),
                    display_value: "/resources/{id}".to_string(),
                    qualifiers_json: json!([{ "key": "method", "value": "PATCH" }]),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    fact_kind: "parameter_name".to_string(),
                    canonical_value_text: "cursor".to_string(),
                    canonical_value_exact: "cursor".to_string(),
                    canonical_value_json: json!({ "value_type": "text", "value": "cursor" }),
                    display_value: "cursor".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
            ],
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "api_guidelines.md".to_string(),
            excerpt: "Cursor pagination uses ?cursor and ?limit.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: repair_technical_layout_noise(
                "Pagination\nUse cursor-based pagination. Query parameters: ?cursor=<token> and ?limit=<int>.",
            ),
        }],
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn build_deterministic_grounded_answer_uses_multi_document_endpoint_facts() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let checkout_revision_id = Uuid::now_v7();
    let rewards_revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::MultiDocument,
            ["current status checkout server", "accounts rewards service"],
            ["endpoint"],
        ),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    fact_kind: "endpoint_path".to_string(),
                    canonical_value_text: "/system/info".to_string(),
                    canonical_value_exact: "/system/info".to_string(),
                    canonical_value_json: json!("/system/info"),
                    display_value: "/system/info".to_string(),
                    qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        checkout_document_id,
                        checkout_revision_id,
                    )
                },
                KnowledgeTechnicalFactRow {
                    fact_kind: "endpoint_path".to_string(),
                    canonical_value_text: "/v1/accounts".to_string(),
                    canonical_value_exact: "/v1/accounts".to_string(),
                    canonical_value_json: json!("/v1/accounts"),
                    display_value: "/v1/accounts".to_string(),
                    qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        rewards_document_id,
                        rewards_revision_id,
                    )
                },
            ],
        },
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
                score: Some(0.94),
                source_text: repair_technical_layout_noise(
                    "/v1/accounts\nGET\nGet Rewards Service account list.",
                ),
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
                score: Some(0.71),
                source_text: repair_technical_layout_noise(
                    "Public checkout server API.\nhttp://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nGet current Checkout Server status.",
                ),
            },
        ],
    )
    .unwrap_or_default();

    assert!(answer.contains("`GET /v1/accounts`"));
    assert!(answer.contains("`GET /system/info`"));
}

#[test]
fn build_deterministic_grounded_answer_uses_port_fact_without_chunk_parsing() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What port does the Rewards Accounts REST API use?",
        &query_ir_with_literals_and_target_types(["rewards accounts rest api"], ["port"]),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "port".to_string(),
                canonical_value_text: "8081".to_string(),
                canonical_value_exact: "8081".to_string(),
                canonical_value_json: json!("8081"),
                display_value: "8081".to_string(),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "rewards_accounts_rest_reference.md".to_string(),
            excerpt: "Rewards Accounts REST API Reference".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.93),
            source_text: "Rewards Accounts REST API Reference".to_string(),
        }],
    )
    .unwrap_or_default();

    assert!(answer.contains("`8081`"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_uses_port_and_protocol_facts() {
    let rewards_document_id = Uuid::now_v7();
    let loyalty_document_id = Uuid::now_v7();
    let rewards_revision_id = Uuid::now_v7();
    let loyalty_revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
        &query_ir_with_scope_literals_and_target_types(
            QueryScope::MultiDocument,
            ["Rewards Accounts REST API", "Customer Profile API"],
            ["port", "protocol"],
        ),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    fact_kind: "port".to_string(),
                    canonical_value_text: "8081".to_string(),
                    canonical_value_exact: "8081".to_string(),
                    canonical_value_json: json!("8081"),
                    display_value: "8081".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        rewards_document_id,
                        rewards_revision_id,
                    )
                },
                KnowledgeTechnicalFactRow {
                    fact_kind: "protocol".to_string(),
                    canonical_value_text: "http".to_string(),
                    canonical_value_exact: "http".to_string(),
                    canonical_value_json: json!("http"),
                    display_value: "http".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        loyalty_document_id,
                        loyalty_revision_id,
                    )
                },
                KnowledgeTechnicalFactRow {
                    fact_kind: "protocol".to_string(),
                    canonical_value_text: "soap".to_string(),
                    canonical_value_exact: "soap".to_string(),
                    canonical_value_json: json!("soap"),
                    display_value: "soap".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        loyalty_document_id,
                        loyalty_revision_id,
                    )
                },
            ],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: rewards_document_id,
                document_label: "rewards_accounts_rest_reference.md".to_string(),
                excerpt: "Rewards Accounts REST API Reference".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: "Rewards Accounts REST API Reference".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: loyalty_document_id,
                document_label: "customer_profile_soap_reference.md".to_string(),
                excerpt: "Customer Profile SOAP API Reference".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.98),
                source_text: "Customer Profile SOAP API Reference".to_string(),
            },
        ],
    )
    .unwrap_or_default();

    assert!(answer.contains("`8081`"), "{answer}");
    assert!(answer.contains("`SOAP`"), "{answer}");
}

#[test]
fn build_deterministic_grounded_answer_skips_port_override_without_fact() {
    let control_document_id = Uuid::now_v7();
    let telegram_document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What port does the Acme Control Center use?",
        &query_ir_with_literals_and_target_types(["Acme Control Center"], ["port"]),
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
                document_id: control_document_id,
                document_label: "Acme Control Center - Example".to_string(),
                excerpt:
                    "Acme Control Center is configuration management software for managed objects."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text: repair_technical_layout_noise(
                    "Acme Control Center\nDescription\nAcme Control Center is configuration management software for managed objects.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: telegram_document_id,
                document_label: "Acme Telegram Bot - Example".to_string(),
                excerpt: "Integration uses localhost:2026.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.91),
                source_text: repair_technical_layout_noise(
                    "Acme Telegram Bot\nSettings\nport: 2026\nlocalhost:2026",
                ),
            },
        ],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_prefers_exact_wsdl_document_over_foreign_focus_noise() {
    let inventory_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let inventory_revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which WSDL does the inventory SOAP API use?",
        &query_ir_with_literals_and_target_types(["inventory soap api"], ["endpoint", "wsdl"]),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "url".to_string(),
                canonical_value_text: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                canonical_value_exact: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                canonical_value_json: json!(
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
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text: "| `pageNumber` | 1-based page number |".to_string(),
                text: "| `pageNumber` | 1-based page number |".to_string(),
                ..sample_structured_block_row(block_id, document_id, revision_id)
            }],
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "parameter_name".to_string(),
                canonical_value_text: "pageNumber".to_string(),
                canonical_value_exact: "pageNumber".to_string(),
                canonical_value_json: json!({ "value_type": "text", "value": "pageNumber" }),
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
fn build_deterministic_grounded_answer_finds_parameter_with_question_mark_despite_foreign_noise() {
    let document_id = Uuid::now_v7();
    let foreign_document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let block_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Does a withCards parameter exist?",
        &query_ir_with_literals_and_target_types(["withCards"], ["parameter"]),
        &CanonicalAnswerEvidence {
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
                canonical_value_text: "withCards".to_string(),
                canonical_value_exact: "withCards".to_string(),
                canonical_value_json: json!({ "value_type": "text", "value": "withCards" }),
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
                document_label: "inventory_soap_api_contract.md".to_string(),
                excerpt: "Inventory SOAP uses WSDL.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "rewards_accounts_api_contract.md".to_string(),
                excerpt: "withCards includes linked card records.".to_string(),
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
        &CanonicalAnswerEvidence {
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
                canonical_value_text: "withCards".to_string(),
                canonical_value_exact: "withCards".to_string(),
                canonical_value_json: json!({ "value_type": "text", "value": "withCards" }),
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

#[test]
fn build_deterministic_grounded_answer_does_not_infer_wsdl_from_chunks_without_fact() {
    let document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which WSDL does the inventory SOAP API use?",
        &fallback_query_ir(),
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
            document_id,
            document_label: "inventory_soap_api_contract.md".to_string(),
            excerpt: "WSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: repair_technical_layout_noise(
                "Inventory SOAP API Contract\nWSDL URL: http://demo.local:8080/inventory-api/ws/inventory.wsdl",
            ),
        }],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_does_not_infer_single_endpoint_from_chunks_without_fact() {
    let document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which endpoint returns current information for the checkout server?",
        &fallback_query_ir(),
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
            document_id,
            document_label: "checkout_runtime_contract.md".to_string(),
            excerpt: "GET /system/info returns current information for the checkout server."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.94),
            source_text: repair_technical_layout_noise(
                "Checkout Runtime Contract\nGET\n/system/info\ncurrent checkout server system information",
            ),
        }],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_does_not_infer_multi_document_endpoints_from_chunks_without_facts()
 {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "If an agent needs the current Checkout Server status and separately the Rewards Service account list, which two endpoints are needed?",
        &fallback_query_ir(),
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
                document_label: "rewards_service_reference.pdf".to_string(),
                excerpt: "GET /v1/accounts returns the Rewards Service account list.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.94),
                source_text: repair_technical_layout_noise(
                    "/v1/accounts\nGET\nGet Rewards Service account list.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: checkout_document_id,
                document_label: "checkout_server_reference.pdf".to_string(),
                excerpt:
                    "To get the current Checkout Server status, send a GET request to /system/info."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.71),
                source_text: repair_technical_layout_noise(
                    "Public checkout server API.\nhttp://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nGet current Checkout Server status.",
                ),
            },
        ],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_does_not_infer_parameter_from_chunks_without_fact() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let block_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Does a withCards parameter exist?",
        &fallback_query_ir(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![KnowledgeStructuredBlockRow {
                normalized_text: "| `withCards` | include linked card records in the response |"
                    .to_string(),
                text: "| `withCards` | include linked card records in the response |".to_string(),
                ..sample_structured_block_row(block_id, document_id, revision_id)
            }],
            technical_facts: Vec::new(),
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
    );

    assert!(answer.is_none());
}
