use super::*;

#[test]
fn transport_config_assignment_answer_requires_assignment_evidence() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "checkout_service_notes.md",
        "The checkout service accepts HTTPS traffic and calls the inventory service on port 9443.",
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Port,
        crate::domains::query_ir::QueryTargetKind::Connection,
    ];

    let answer = build_transport_config_assignment_answer(
        "Which ports and connections does the checkout service require?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "transport assignment rendering requires concrete config assignments: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_rejects_a_ranked_source_without_question_focus() {
    let distractor_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let chunks = vec![
        runtime_chunk(
            distractor_document_id,
            0,
            "framework_setup.md",
            "entrypoint = package.main\nmode = production\nworkers = 4",
        ),
        runtime_chunk(
            target_document_id,
            0,
            "service_manifest.yaml",
            "DATABASE_URL: protocol://database:5432/sample",
        ),
    ];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Connection,
        crate::domains::query_ir::QueryTargetKind::ConfigKey,
    ];
    query_ir.target_entities =
        vec![EntityMention { label: "database connection".to_string(), role: EntityRole::Subject }];
    query_ir.literal_constraints = vec![literal_constraint("DATABASE_URL")];
    query_ir.retrieval_query = Some("database connection DATABASE_URL 5432".to_string());

    let answer = build_transport_config_assignment_answer(
        "Return the database connection setting.",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "the assignment builder must decline when only another document carries the typed question focus: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_abstains_for_compound_port_inventory() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "alpha_records.txt",
        r#"
entity.alpha = alpha
entity.beta = beta
entity.updated_at = now()
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
        crate::domains::query_ir::QueryTargetKind::Port,
        crate::domains::query_ir::QueryTargetKind::Procedure,
    ];
    query_ir.target_entities =
        vec![EntityMention { label: "Alpha Records".to_string(), role: EntityRole::Subject }];

    let answer = build_transport_config_assignment_answer(
        "Which port values does Alpha Records expose?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "compound port inventory should not be answered by assignment-shaped rows: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_abstains_for_broad_protocol_explanation() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "neighboring_config.txt",
        r#"
service.endpoint = https://example.invalid:9443
service.timeout = 30
service.enabled = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Protocol,
        crate::domains::query_ir::QueryTargetKind::Concept,
    ];

    let answer = build_transport_config_assignment_answer(
        "What are the main improvements of Protocol X version 2 over version 1?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "broad protocol/concept questions should be synthesized from evidence, not rendered as config assignments: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_requires_connection_or_configuration_target() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "neighboring_config.txt",
        r#"
listener.protocol = alpha
listener.timeout = 30
listener.enabled = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Protocol];

    let answer = build_transport_config_assignment_answer(
        "Which protocol is described?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "a protocol-only value lookup must not infer a config-assignment answer without a connection/config target: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_ranks_typed_facts_when_the_winner_fits_the_question() {
    let distractor_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let distractor = runtime_chunk(
        distractor_document_id,
        0,
        "first_source.cfg",
        r#"
port = 9000
url = https://distractor.invalid:9000
timeout = 30
"#,
    );
    let target = runtime_chunk(
        target_document_id,
        0,
        "typed_source.cfg",
        r#"
keyAlpha = https://target.invalid:9443
keyBeta = 9443
"#,
    );
    let mut url_fact = crate::services::query::execution::sample_technical_fact_row(
        Uuid::now_v7(),
        target.document_id,
        target.revision_id,
    );
    url_fact.fact_kind = "url".to_string();
    url_fact.display_value = "https://target.invalid:9443".to_string();
    url_fact.normalized_value_text = url_fact.display_value.clone();
    url_fact.normalized_value_exact = url_fact.display_value.clone();
    url_fact.support_chunk_ids = vec![target.chunk_id];
    let mut port_fact = crate::services::query::execution::sample_technical_fact_row(
        Uuid::now_v7(),
        target.document_id,
        target.revision_id,
    );
    port_fact.fact_kind = "port".to_string();
    port_fact.display_value = "9443".to_string();
    port_fact.normalized_value_text = port_fact.display_value.clone();
    port_fact.normalized_value_exact = port_fact.display_value.clone();
    port_fact.support_chunk_ids = vec![target.chunk_id];
    let evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: vec![url_fact, port_fact],
    };
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Connection,
        crate::domains::query_ir::QueryTargetKind::Url,
        crate::domains::query_ir::QueryTargetKind::Port,
    ];
    // The builder now requires the selected source to answer the typed focus;
    // typed fact scoring ranks only candidates that clear that invariant.
    query_ir.target_entities =
        vec![EntityMention { label: "typed source".to_string(), role: EntityRole::Subject }];

    let answer = build_exact_technical_literal_answer(
        "Return the configured transport values.",
        &query_ir,
        &evidence,
        &[distractor, target],
    )
    .expect("typed assignment answer");

    assert!(answer.contains("`typed_source.cfg`"), "{answer}");
    assert!(!answer.contains("`first_source.cfg`"), "{answer}");
}
