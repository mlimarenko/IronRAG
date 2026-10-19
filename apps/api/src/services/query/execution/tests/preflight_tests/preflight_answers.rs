use super::*;

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
    let revision_id = Uuid::now_v7();
    let answer = build_evidence_preflight_answer(
        "Which endpoint returns the current checkout server info?",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        None,
        &single_endpoint_answer_evidence(document_id, revision_id),
        &merged,
    )
    .expect("single endpoint preflight answer from merged candidates");

    assert_eq!(answer.answer, "The endpoint is `GET /system/info`.");
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
        "Install module with sample-install alpha-connector and configure /opt/alpha/alpha.conf.",
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
fn empty_setup_preflight_recovers_only_fixed_setup_lane_context() {
    let retrieved_document_id = Uuid::now_v7();
    let context_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: retrieved_document_id,
        document_label: "sample-provider-setup.md".to_string(),
        excerpt: "Install the module and configure its documented parameters.".to_string(),
        score_kind: RuntimeChunkScoreKind::DocumentIdentity,
        score: Some(0.9),
        source_text: "Install the module and configure its documented parameters.".to_string(),
    };
    let distractor = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "unrelated-overview.md".to_string(),
        excerpt: "Unrelated primary retrieval tail.".to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(0.8),
        source_text: "Unrelated primary retrieval tail.".to_string(),
    };
    let mut preflight_chunks = Vec::new();

    extend_setup_preflight_chunks_from_setup_lanes(
        &mut preflight_chunks,
        &[context_chunk.clone(), distractor],
    );

    assert_eq!(preflight_chunks.len(), 1);
    assert_eq!(preflight_chunks[0].chunk_id, context_chunk.chunk_id);
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
    let mut evidence = AuthoritativeAnswerEvidence {
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
fn evidence_preflight_answer_keeps_follow_up_setup_questions_generative() {
    let document_id = Uuid::now_v7();
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
sample-install alpha-connector

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
    let evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };
    let initial_answer = build_evidence_preflight_answer(
        "How do I configure Provider Alpha?",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        None,
        &evidence,
        &chunks,
    )
    .expect("initial setup preflight answer");

    assert!(initial_answer.answer.contains("`alpha-connector`"), "{initial_answer:?}");
    query_ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "those settings".to_string(),
    }];

    let follow_up_answer = build_evidence_preflight_answer(
        "Explain those settings",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        None,
        &evidence,
        &chunks,
    );

    assert!(follow_up_answer.is_none());
}

#[test]
fn evidence_preflight_answer_keeps_scoped_setup_inventory_generative() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id,
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
sample-install alpha-connector

Connector settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
Row 1 | Name: alphaTimeout | Type: integer | Default: 10 | Description: Poll interval.
"#,
        ),
    }];
    let query_ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["Provider Alpha", "alphaTimeout"],
        ["package", "configuration_file", "parameter"],
    );
    let evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: vec![KnowledgeStructuredBlockRow {
            normalized_text: "Row 1 | Name: alphaTimeout | Type: integer | Default: 10 | Description: Poll interval."
                .to_string(),
            text: "Row 1 | Name: alphaTimeout | Type: integer | Default: 10 | Description: Poll interval."
                .to_string(),
            ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
        }],
        technical_facts: Vec::new(),
    };
    let scoped_question = "scope: previous answer mentioned package and configuration literals\nquestion: explain how to configure all settings";

    let deterministic =
        build_deterministic_grounded_answer(scoped_question, &query_ir, &evidence, &chunks)
            .expect("deterministic setup answer without preflight guard");
    assert!(deterministic.contains("`alpha-connector`"), "{deterministic}");

    let answer = build_evidence_preflight_answer(
        scoped_question,
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        None,
        &evidence,
        &chunks,
    );

    assert!(answer.is_none());
}

#[test]
fn evidence_preflight_answer_preserves_scoped_exact_setup_literal_answers() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id,
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "Provider Alpha setup".to_string(),
        excerpt: "Connector parameter table.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.9),
        source_text: repair_technical_layout_noise(
            "Sheet: Connector settings | Row 1 | Name: alphaTimeout | Type: integer | Default: 10 | Description: Poll interval",
        ),
    }];
    let query_ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["Provider Alpha", "alphaTimeout"],
        ["parameter"],
    );
    let evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: vec![KnowledgeStructuredBlockRow {
            normalized_text: "| `alphaTimeout` | integer | Default: 10 | Poll interval |"
                .to_string(),
            text: "| `alphaTimeout` | integer | Default: 10 | Poll interval |".to_string(),
            ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
        }],
        technical_facts: vec![KnowledgeTechnicalFactRow {
            fact_kind: "parameter_name".to_string(),
            normalized_value_text: "alphaTimeout".to_string(),
            normalized_value_exact: "alphaTimeout".to_string(),
            normalized_value_json: json!({ "value_type": "text", "value": "alphaTimeout" }),
            display_value: "alphaTimeout".to_string(),
            ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
        }],
    };
    let scoped_question = "scope: previous answer mentioned several connector settings\nquestion: explain alphaTimeout";

    let answer = build_evidence_preflight_answer(
        scoped_question,
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        None,
        &evidence,
        &chunks,
    )
    .expect("scoped exact setup literal answer");

    assert!(answer.answer.contains("`alphaTimeout`"), "{answer:?}");
    assert!(answer.answer.contains("integer"), "{answer:?}");
}

#[test]
fn evidence_preflight_answer_preserves_direct_table_follow_up_answers() {
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
    let evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };
    let answer = build_evidence_preflight_answer(
        "Show the same table rows",
        &query_ir,
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        Some("direct table answer".to_string()),
        &evidence,
        &[],
    )
    .expect("direct table follow-up answer");

    assert_eq!(answer.answer, "direct table answer");
}
