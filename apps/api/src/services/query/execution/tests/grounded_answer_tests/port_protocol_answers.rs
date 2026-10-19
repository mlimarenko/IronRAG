use super::*;

#[test]
fn build_deterministic_grounded_answer_uses_port_fact_without_chunk_parsing() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What port does the Rewards Accounts REST API use?",
        &query_ir_with_literals_and_target_types(["rewards accounts rest api"], ["port"]),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![KnowledgeTechnicalFactRow {
                fact_kind: "port".to_string(),
                normalized_value_text: "8081".to_string(),
                normalized_value_exact: "8081".to_string(),
                normalized_value_json: json!("8081"),
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
    let profile_document_id = Uuid::now_v7();
    let rewards_revision_id = Uuid::now_v7();
    let profile_revision_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
        &query_ir_with_scope_literals_and_target_types(
            QueryScope::MultiDocument,
            ["Rewards Accounts REST API", "Customer Profile API"],
            ["port", "protocol"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    fact_kind: "port".to_string(),
                    normalized_value_text: "8081".to_string(),
                    normalized_value_exact: "8081".to_string(),
                    normalized_value_json: json!("8081"),
                    display_value: "8081".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        rewards_document_id,
                        rewards_revision_id,
                    )
                },
                KnowledgeTechnicalFactRow {
                    fact_kind: "protocol".to_string(),
                    normalized_value_text: "soap".to_string(),
                    normalized_value_exact: "soap".to_string(),
                    normalized_value_json: json!("soap"),
                    display_value: "soap".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        profile_document_id,
                        profile_revision_id,
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
                document_id: profile_document_id,
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
fn build_deterministic_grounded_answer_abstains_from_ranking_ambiguous_protocol_facts() {
    let port_document_id = Uuid::now_v7();
    let protocol_document_id = Uuid::now_v7();
    let port_revision_id = Uuid::now_v7();
    let protocol_revision_id = Uuid::now_v7();
    let fact = |kind: &str, value: &str, document_id, revision_id| KnowledgeTechnicalFactRow {
        fact_kind: kind.to_string(),
        normalized_value_text: value.to_string(),
        normalized_value_exact: value.to_string(),
        normalized_value_json: json!(value),
        display_value: value.to_string(),
        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
    };

    let answer = build_deterministic_grounded_answer(
        "Return the port of Service Alpha and the protocol of Service Beta.",
        &query_ir_with_scope_literals_and_target_types(
            QueryScope::MultiDocument,
            ["Service Alpha", "Service Beta"],
            ["port", "protocol"],
        ),
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                fact("port", "8081", port_document_id, port_revision_id),
                fact("protocol", "http", protocol_document_id, protocol_revision_id),
                fact("protocol", "soap", protocol_document_id, protocol_revision_id),
            ],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: port_revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_id: port_document_id,
                document_label: "Service Alpha reference".to_string(),
                excerpt: "Service Alpha reference".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.99),
                source_text: "Service Alpha reference".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: protocol_revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_id: protocol_document_id,
                document_label: "Service Beta reference".to_string(),
                excerpt: "Service Beta reference".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.98),
                source_text: "Service Beta reference".to_string(),
            },
        ],
    );

    assert!(answer.is_none(), "ambiguous protocol facts must reach grounded synthesis");
}

#[test]
fn build_deterministic_grounded_answer_skips_port_override_without_fact() {
    let control_document_id = Uuid::now_v7();
    let telegram_document_id = Uuid::now_v7();

    let answer = build_deterministic_grounded_answer(
        "What port does the Acme Sample Console use?",
        &query_ir_with_literals_and_target_types(["Acme Sample Console"], ["port"]),
        &AuthoritativeAnswerEvidence {
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
                document_label: "Acme Sample Console - Example".to_string(),
                excerpt:
                    "Acme Sample Console is configuration management software for managed objects."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text: repair_technical_layout_noise(
                    "Acme Sample Console\nDescription\nAcme Sample Console is configuration management software for managed objects.",
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
