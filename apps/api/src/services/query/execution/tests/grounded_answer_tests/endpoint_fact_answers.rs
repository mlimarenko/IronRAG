use super::*;

fn endpoint_block_competition_fixture(
    endpoint_text: &str,
) -> (AuthoritativeAnswerEvidence, RuntimeMatchedChunk) {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let paragraph_block_id = Uuid::now_v7();
    let endpoint_block_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let paragraph_text = "The Subject Alpha gateway public API listener at edge-alpha.example:7443 is the only endpoint reachable from outside the perimeter.";

    let mut paragraph_block =
        sample_structured_block_row(paragraph_block_id, document_id, revision_id);
    paragraph_block.ordinal = 10;
    paragraph_block.text = paragraph_text.to_string();
    paragraph_block.normalized_text = paragraph_text.to_string();
    paragraph_block.heading_trail = vec!["Listener interfaces".to_string()];
    paragraph_block.section_path = vec!["listener-interfaces".to_string()];

    let mut endpoint_block =
        sample_structured_block_row(endpoint_block_id, document_id, revision_id);
    endpoint_block.ordinal = 14;
    endpoint_block.block_kind = "endpoint_block".to_string();
    endpoint_block.text = endpoint_text.to_string();
    endpoint_block.normalized_text = endpoint_text.to_string();
    endpoint_block.heading_trail = vec!["Operational checks".to_string()];
    endpoint_block.section_path = vec!["operational-checks".to_string()];

    let mut chunk_row = sample_chunk_row(chunk_id, document_id, revision_id);
    chunk_row.chunk_kind = Some("endpoint_block".to_string());
    chunk_row.content_text = format!("{paragraph_text}\n\n{endpoint_text}");
    chunk_row.normalized_text = chunk_row.content_text.clone();
    chunk_row.support_block_ids = vec![paragraph_block_id, endpoint_block_id];

    let fact = KnowledgeTechnicalFactRow {
        fact_kind: "endpoint_path".to_string(),
        normalized_value_text: "/status/ready".to_string(),
        normalized_value_exact: "/status/ready".to_string(),
        normalized_value_json: json!("/status/ready"),
        display_value: "/status/ready".to_string(),
        support_block_ids: vec![endpoint_block_id],
        support_chunk_ids: vec![chunk_id],
        extraction_kind: "parser_endpoint_block_endpoint_path".to_string(),
        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
    };
    let runtime_chunk = RuntimeMatchedChunk {
        chunk_id,
        revision_id,
        chunk_index: 0,
        chunk_kind: Some("endpoint_block".to_string()),
        document_id,
        document_label: "subject_alpha_gateway_matrix.md".to_string(),
        excerpt: paragraph_text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.98),
        source_text: chunk_row.content_text.clone(),
    };

    (
        AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: vec![chunk_row],
            structured_blocks: vec![paragraph_block, endpoint_block],
            technical_facts: vec![fact],
        },
        runtime_chunk,
    )
}

#[test]
fn endpoint_fact_yields_when_a_sibling_block_better_covers_the_ask() {
    let endpoint_text = "/status/ready is served at admin-alpha.example:9443 for readiness checks. It is never served on the Subject Alpha public API listener.";
    let (evidence, chunk) = endpoint_block_competition_fixture(endpoint_text);

    let answer = build_deterministic_grounded_answer(
        "Which endpoint serves the Subject Alpha gateway public API, and which listener is reachable from outside the perimeter?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["endpoint", "network", "service"],
        ),
        &evidence,
        &[chunk],
    );

    assert!(answer.is_none(), "a lower-coverage typed fact must yield to grounded synthesis");
}

#[test]
fn endpoint_fact_resolves_when_its_block_best_covers_the_ask() {
    let endpoint_text = "/status/ready is the Subject Alpha gateway readiness endpoint served on the admin listener at admin-alpha.example:9443.";
    let (evidence, mut chunk) = endpoint_block_competition_fixture(endpoint_text);
    chunk.chunk_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which readiness endpoint does the Subject Alpha gateway serve on its admin listener?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["endpoint", "network", "service"],
        ),
        &evidence,
        &[chunk],
    );

    assert_eq!(answer.as_deref(), Some("The endpoint is `/status/ready`."));
}

#[test]
fn endpoint_fact_support_terms_break_same_kind_ties() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let preferred_block_id = Uuid::now_v7();
    let other_block_id = Uuid::now_v7();
    let preferred_chunk_id = Uuid::now_v7();
    let other_chunk_id = Uuid::now_v7();
    let preferred_text = "/v2/records is the North Array gateway public endpoint.";
    let other_text = "/status/ready is the endpoint the public API does serve.";

    let mut preferred_block =
        sample_structured_block_row(preferred_block_id, document_id, revision_id);
    preferred_block.block_kind = "endpoint_block".to_string();
    preferred_block.text = preferred_text.to_string();
    preferred_block.normalized_text = preferred_text.to_string();
    preferred_block.heading_trail = vec!["Interfaces".to_string()];
    preferred_block.section_path = vec!["interfaces".to_string()];

    let mut other_block = sample_structured_block_row(other_block_id, document_id, revision_id);
    other_block.block_kind = "endpoint_block".to_string();
    other_block.text = other_text.to_string();
    other_block.normalized_text = other_text.to_string();
    other_block.heading_trail = vec!["Interfaces".to_string()];
    other_block.section_path = vec!["interfaces".to_string()];

    let mut preferred_chunk = sample_chunk_row(preferred_chunk_id, document_id, revision_id);
    preferred_chunk.chunk_kind = Some("endpoint_block".to_string());
    preferred_chunk.content_text = preferred_text.to_string();
    preferred_chunk.normalized_text = preferred_text.to_string();
    preferred_chunk.support_block_ids = vec![preferred_block_id];

    let mut other_chunk = sample_chunk_row(other_chunk_id, document_id, revision_id);
    other_chunk.chunk_kind = Some("endpoint_block".to_string());
    other_chunk.content_text = other_text.to_string();
    other_chunk.normalized_text = other_text.to_string();
    other_chunk.support_block_ids = vec![other_block_id];

    let preferred_fact = KnowledgeTechnicalFactRow {
        normalized_value_text: "/v2/records".to_string(),
        normalized_value_exact: "/v2/records".to_string(),
        normalized_value_json: json!("/v2/records"),
        display_value: "/v2/records".to_string(),
        support_block_ids: vec![preferred_block_id],
        support_chunk_ids: vec![preferred_chunk_id],
        extraction_kind: "parser_endpoint_block_endpoint_path".to_string(),
        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
    };
    let other_fact = KnowledgeTechnicalFactRow {
        normalized_value_text: "/status/ready".to_string(),
        normalized_value_exact: "/status/ready".to_string(),
        normalized_value_json: json!("/status/ready"),
        display_value: "/status/ready".to_string(),
        support_block_ids: vec![other_block_id],
        support_chunk_ids: vec![other_chunk_id],
        extraction_kind: "parser_endpoint_block_endpoint_path".to_string(),
        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
    };
    let runtime_chunks = [
        RuntimeMatchedChunk {
            chunk_id: preferred_chunk_id,
            revision_id,
            chunk_index: 0,
            chunk_kind: Some("endpoint_block".to_string()),
            document_id,
            document_label: "interface_matrix.md".to_string(),
            excerpt: preferred_text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: preferred_text.to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: other_chunk_id,
            revision_id,
            chunk_index: 1,
            chunk_kind: Some("endpoint_block".to_string()),
            document_id,
            document_label: "interface_matrix.md".to_string(),
            excerpt: other_text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: other_text.to_string(),
        },
    ];

    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["endpoint", "network", "service"],
    );
    query_ir.target_entities =
        vec![EntityMention { label: "North Array gateway".to_string(), role: EntityRole::Subject }];

    let answer = build_deterministic_grounded_answer(
        "Which endpoint does the North Array gateway public API serve?",
        &query_ir,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: vec![preferred_chunk, other_chunk],
            structured_blocks: vec![preferred_block, other_block],
            technical_facts: vec![preferred_fact, other_fact],
        },
        &runtime_chunks,
    );

    assert_eq!(answer.as_deref(), Some("The endpoint is `/v2/records`."));
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
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["inventory soap api"],
            ["url", "wsdl"],
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
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["checkout server", "current information"],
            ["endpoint"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "endpoint_path".to_string(),
                normalized_value_text: "/system/info".to_string(),
                normalized_value_exact: "/system/info".to_string(),
                normalized_value_json: json!("/system/info"),
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
fn build_deterministic_grounded_answer_abstains_when_endpoint_fact_lacks_query_focus_support() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which response window applies to SevOne incidents?",
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["SevOne incidents", "response window"],
            ["endpoint"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "endpoint_path".to_string(),
                normalized_value_text: "/health".to_string(),
                normalized_value_exact: "/health".to_string(),
                normalized_value_json: json!("/health"),
                display_value: "/health".to_string(),
                qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "incident_response.md".to_string(),
                excerpt: "GET /health returns service status.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.97),
                source_text: repair_technical_layout_noise(
                    "Health Check\nGET /health returns service status.",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id,
                chunk_index: 1,
                chunk_kind: None,
                document_id,
                document_label: "incident_response.md".to_string(),
                excerpt: "SevOne incidents use a 15 minute response window.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text: repair_technical_layout_noise(
                    "Incident Matrix\nSevOne incidents use a 15 minute response window.",
                ),
            },
        ],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_skips_graph_namespace_endpoint_for_graph_route_literal_signal()
 {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["route"],
    );
    query_ir
        .literal_constraints
        .push(LiteralSpan { text: "/system/info".to_string(), kind: LiteralKind::Path });

    let answer = build_deterministic_grounded_answer(
        "Which route path handles the status check?",
        &query_ir,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    fact_kind: "endpoint_path".to_string(),
                    normalized_value_text: "/wiki/Knowledge_graph".to_string(),
                    normalized_value_exact: "/wiki/Knowledge_graph".to_string(),
                    normalized_value_json: json!("/wiki/Knowledge_graph"),
                    display_value: "/wiki/Knowledge_graph".to_string(),
                    qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    fact_kind: "endpoint_path".to_string(),
                    normalized_value_text: "/system/info".to_string(),
                    normalized_value_exact: "/system/info".to_string(),
                    normalized_value_json: json!("/system/info"),
                    display_value: "/system/info".to_string(),
                    qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
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
            document_label: "entity_route_graph.md".to_string(),
            excerpt: "Entity route reference".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: repair_technical_layout_noise(
                "Entity routing table includes /wiki/Knowledge_graph and GET /system/info references.",
            ),
        }],
    )
    .unwrap_or_default();

    assert_eq!(answer, "The endpoint is `GET /system/info`.");
}

#[test]
fn build_deterministic_grounded_answer_abstains_from_graph_namespace_endpoint_when_only_generic_endpoint_target_is_present()
 {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which endpoint handles this entity route?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["endpoint", "entity"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "endpoint_path".to_string(),
                normalized_value_text: "/wiki/Knowledge_graph".to_string(),
                normalized_value_exact: "/wiki/Knowledge_graph".to_string(),
                normalized_value_json: json!("/wiki/Knowledge_graph"),
                display_value: "/wiki/Knowledge_graph".to_string(),
                qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "entity_route_graph.md".to_string(),
            excerpt: "Entity route reference".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: repair_technical_layout_noise(
                "Entity routing table includes GET /wiki/Knowledge_graph for primary metadata.",
            ),
        }],
    );

    assert!(answer.is_none());
}

#[test]
fn build_deterministic_grounded_answer_keeps_graph_namespace_endpoint_for_url_target() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "Which API endpoint handles this entity route?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["url", "entity"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "endpoint_path".to_string(),
                normalized_value_text: "/wiki/Knowledge_graph".to_string(),
                normalized_value_exact: "/wiki/Knowledge_graph".to_string(),
                normalized_value_json: json!("/wiki/Knowledge_graph"),
                display_value: "/wiki/Knowledge_graph".to_string(),
                qualifiers_json: json!([{ "key": "method", "value": "GET" }]),
                ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
            }],
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "entity_route_graph.md".to_string(),
            excerpt: "Entity route API reference".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.98),
            source_text: repair_technical_layout_noise(
                "Entity route API reference includes GET /wiki/Knowledge_graph for primary metadata.",
            ),
        }],
    )
    .unwrap_or_default();

    assert_eq!(answer, "The endpoint is `GET /wiki/Knowledge_graph`.");
}

#[test]
fn build_deterministic_grounded_answer_abstains_when_endpoint_candidate_misses_parameter_facet() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What pagination approach is recommended, and what query parameters are used?",
        &query_ir_with_act_scope_literals_and_target_types(
            QueryAct::RetrieveValue,
            QueryScope::SingleDocument,
            ["pagination query parameters"],
            ["endpoint", "parameter"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    fact_kind: "endpoint_path".to_string(),
                    normalized_value_text: "/resources/{id}".to_string(),
                    normalized_value_exact: "/resources/{id}".to_string(),
                    normalized_value_json: json!("/resources/{id}"),
                    display_value: "/resources/{id}".to_string(),
                    qualifiers_json: json!([{ "key": "method", "value": "PATCH" }]),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    fact_kind: "parameter_name".to_string(),
                    normalized_value_text: "cursor".to_string(),
                    normalized_value_exact: "cursor".to_string(),
                    normalized_value_json: json!({ "value_type": "text", "value": "cursor" }),
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
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    fact_kind: "endpoint_path".to_string(),
                    normalized_value_text: "/system/info".to_string(),
                    normalized_value_exact: "/system/info".to_string(),
                    normalized_value_json: json!("/system/info"),
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
                    normalized_value_text: "/v1/accounts".to_string(),
                    normalized_value_exact: "/v1/accounts".to_string(),
                    normalized_value_json: json!("/v1/accounts"),
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
