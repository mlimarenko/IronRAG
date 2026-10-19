use super::*;

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
    let preflight_evidence = build_preflight_authoritative_evidence(
        question,
        &ir,
        &profile,
        &AuthoritativeAnswerEvidence {
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
fn preflight_exact_literal_scope_stays_broad_for_low_confidence_unfocused_descriptive_ir() {
    let screen_document_id = Uuid::now_v7();
    let guide_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::Describe,
        QueryScope::SingleDocument,
        [],
    );
    ir.confidence = 0.25;
    let technical_literal_chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: screen_document_id,
            document_label: "checkout-payment-screen.png".to_string(),
            excerpt: "Terminal screen shows an interrupted payment confirmation flow."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: "Terminal screen shows an interrupted payment confirmation flow."
                .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: guide_document_id,
            document_label: "provider-alpha-payment-guide.md".to_string(),
            excerpt: "The operator repeats status checks before closing the payment flow."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
            score: Some(0.74),
            source_text:
                "The operator repeats status checks before closing the payment flow. If confirmation is still absent, the operator follows the documented reversal path."
                    .to_string(),
        },
    ];

    assert_eq!(
        preflight_exact_literal_document_scope(
            "What should the operator do when QX payment confirmation is missing?",
            &ir,
            &profile,
            &technical_literal_chunks,
        ),
        None
    );
}

#[test]
fn preflight_exact_literal_scope_stays_broad_for_open_descriptive_context() {
    let screen_document_id = Uuid::now_v7();
    let guide_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::Describe,
        QueryScope::SingleDocument,
        ["concept"],
    );
    ir.document_focus =
        Some(DocumentHint { hint: "Provider Alpha terminal payment confirmation".to_string() });
    ir.target_entities = vec![
        EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject },
        EntityMention { label: "terminal".to_string(), role: EntityRole::Subject },
        EntityMention { label: "payment confirmation".to_string(), role: EntityRole::Object },
    ];
    let technical_literal_chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: screen_document_id,
            document_label: "checkout-payment-screen.png".to_string(),
            excerpt: "Payment type selector autonomous terminal provider alpha".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.99),
            source_text: "Payment type selector autonomous terminal provider alpha".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: guide_document_id,
            document_label: "provider-alpha-payment-guide.md".to_string(),
            excerpt: "Successful payment confirmation is received from provider alpha."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.74),
            source_text:
                "Successful payment confirmation is received from provider alpha. Repeat status checks before closing the payment flow."
                    .to_string(),
        },
    ];

    assert_eq!(
        preflight_exact_literal_document_scope(
            "What should the operator do when payment confirmation is missing?",
            &ir,
            &profile,
            &technical_literal_chunks,
        ),
        None
    );
}
