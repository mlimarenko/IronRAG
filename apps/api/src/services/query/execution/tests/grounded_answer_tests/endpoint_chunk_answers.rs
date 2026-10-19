use super::*;

#[test]
fn build_multi_document_endpoint_answer_handles_english_checkout_rewards_question() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();

    let answer = build_multi_document_endpoint_answer_from_chunks(
            "If an agent needs the current Checkout Server status and then the Rewards Accounts list, which two endpoints should it call?",
            &query_ir_with_act_scope_literals_and_target_types(
                QueryAct::RetrieveValue,
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
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["current information", "checkout server"],
            ["endpoint"],
        ),
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
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
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
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
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
            &query_ir_with_act_scope_literals_and_target_types(
                QueryAct::RetrieveValue,
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
            &query_ir_with_act_scope_literals_and_target_types(
                QueryAct::RetrieveValue,
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
