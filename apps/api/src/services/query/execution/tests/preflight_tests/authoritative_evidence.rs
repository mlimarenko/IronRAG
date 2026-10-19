use super::*;

#[test]
fn build_preflight_authoritative_evidence_scopes_exact_literal_questions_to_literal_documents() {
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let rewards_revision_id = Uuid::now_v7();
    let inventory_revision_id = Uuid::now_v7();
    let filtered = build_preflight_authoritative_evidence(
        "What is the pageNumber parameter called in the pagination API?",
        &query_ir_with_literals_and_target_types(["pageNumber"], ["parameter"]),
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &AuthoritativeAnswerEvidence {
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
fn build_preflight_authoritative_evidence_keeps_corpus_for_follow_up_when_literal_scope_has_no_overlap()
 {
    let primary_document_id = Uuid::now_v7();
    let scoped_document_id = Uuid::now_v7();
    let current_revision_id = Uuid::now_v7();
    let scoped_revision_id = Uuid::now_v7();
    let question = "How should Provider Alpha runtime settings be configured?";
    let mut ir = query_ir_with_act_scope_literals_and_target_types(
        QueryAct::ConfigureHow,
        QueryScope::SingleDocument,
        ["Provider Alpha", "runtime settings"],
        ["configuration", "parameter"],
    );
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let authoritative_evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: vec![sample_chunk_row(
            Uuid::now_v7(),
            primary_document_id,
            current_revision_id,
        )],
        structured_blocks: vec![KnowledgeStructuredBlockRow {
            normalized_text: "Provider Alpha runtime settings use alpha.ini [Main].".to_string(),
            text: "Provider Alpha runtime settings use alpha.ini [Main].".to_string(),
            ..sample_structured_block_row(Uuid::now_v7(), primary_document_id, current_revision_id)
        }],
        technical_facts: vec![KnowledgeTechnicalFactRow {
            fact_kind: "configuration_file".to_string(),
            normalized_value_text: "alpha.ini".to_string(),
            normalized_value_exact: "alpha.ini".to_string(),
            normalized_value_json: json!("alpha.ini"),
            display_value: "alpha.ini".to_string(),
            ..sample_technical_fact_row(Uuid::now_v7(), primary_document_id, current_revision_id)
        }],
    };
    let technical_literal_chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: scoped_revision_id,
        chunk_index: 0,
        chunk_kind: None,
        document_id: scoped_document_id,
        document_label: "provider_alpha_release_notes.md".to_string(),
        excerpt: "Provider Alpha runtime settings are mentioned in release notes.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.99),
        source_text: "Provider Alpha runtime settings are mentioned in release notes.".to_string(),
    }];

    let scoped_documents =
        preflight_exact_literal_document_scope(question, &ir, &profile, &technical_literal_chunks)
            .expect("literal scope");
    assert_eq!(scoped_documents, HashSet::from([scoped_document_id]));

    let non_follow_up_evidence = build_preflight_authoritative_evidence(
        question,
        &ir,
        &profile,
        &authoritative_evidence,
        &technical_literal_chunks,
    );
    assert!(non_follow_up_evidence.chunk_rows.is_empty());
    assert!(non_follow_up_evidence.structured_blocks.is_empty());
    assert!(non_follow_up_evidence.technical_facts.is_empty());

    let effective_question = format!(
        "scope: Provider Alpha setup was selected in the previous turn\nquestion: {question}"
    );
    let structured_non_follow_up_evidence = build_preflight_authoritative_evidence(
        &effective_question,
        &ir,
        &profile,
        &authoritative_evidence,
        &technical_literal_chunks,
    );
    assert!(structured_non_follow_up_evidence.chunk_rows.is_empty());
    assert!(structured_non_follow_up_evidence.structured_blocks.is_empty());
    assert!(structured_non_follow_up_evidence.technical_facts.is_empty());

    ir.conversation_refs = vec![crate::domains::query_ir::UnresolvedRef {
        kind: crate::domains::query_ir::ConversationRefKind::Elliptic,
        surface: "those settings".to_string(),
    }];
    let preflight_evidence = build_preflight_authoritative_evidence(
        question,
        &ir,
        &profile,
        &authoritative_evidence,
        &technical_literal_chunks,
    );

    assert_eq!(
        preflight_evidence.chunk_rows.iter().map(|row| row.document_id).collect::<HashSet<_>>(),
        HashSet::from([primary_document_id])
    );
    assert_eq!(
        preflight_evidence
            .structured_blocks
            .iter()
            .map(|block| block.document_id)
            .collect::<HashSet<_>>(),
        HashSet::from([primary_document_id])
    );
    assert_eq!(
        preflight_evidence
            .technical_facts
            .iter()
            .map(|fact| fact.document_id)
            .collect::<HashSet<_>>(),
        HashSet::from([primary_document_id])
    );
}

#[test]
fn focused_document_preflight_ignores_spurious_exact_literal_document_scope() {
    let pdf_document_id = Uuid::now_v7();
    let docx_document_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let mut ir = query_ir_with_act_scope_and_target_types(
        QueryAct::RetrieveValue,
        QueryScope::SingleDocument,
        ["secondary_heading", "config_key"],
    );
    ir.literal_constraints.push(LiteralSpan {
        text: "upload_smoke_fixture.docx".to_string(),
        kind: LiteralKind::Path,
    });
    ir.literal_constraints.push(LiteralSpan {
        text: "runtime PDF upload check".to_string(),
        kind: LiteralKind::Identifier,
    });
    let pdf_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: pdf_document_id,
        document_label: "runtime_upload_check.pdf".to_string(),
        excerpt: "Runtime PDF upload check".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.7),
        source_text: "Runtime PDF upload check\n\nQuarterly graph report".to_string(),
    };
    let docx_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: docx_document_id,
        document_label: "runtime_upload_check.docx".to_string(),
        excerpt: "Runtime DOCX upload check".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.95),
        source_text: "Runtime DOCX upload check\n\nPrimary pipeline validation".to_string(),
    };

    assert!(
        preflight_exact_literal_document_scope(
            "What report name appears in the runtime PDF upload check?",
            &ir,
            &profile,
            std::slice::from_ref(&docx_chunk),
        )
        .is_none()
    );

    let chunks = build_preflight_answer_chunks(
        "What report name appears in the runtime PDF upload check?",
        &ir,
        &profile,
        std::slice::from_ref(&pdf_chunk),
        std::slice::from_ref(&docx_chunk),
    );
    let answer = build_evidence_preflight_answer(
        "What report name appears in the runtime PDF upload check?",
        &ir,
        &profile,
        None,
        &AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &chunks,
    )
    .expect("focused document preflight answer");

    assert_eq!(answer.answer, "Quarterly graph report");
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
    let context = build_authoritative_answer_context(
        question,
        &ir,
        None,
        &build_preflight_authoritative_evidence(
            question,
            &ir,
            &profile,
            &AuthoritativeAnswerEvidence {
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
fn evidence_preflight_answer_uses_literal_scoped_evidence_for_parameter_question() {
    let rewards_document_id = Uuid::now_v7();
    let inventory_document_id = Uuid::now_v7();
    let rewards_revision_id = Uuid::now_v7();
    let inventory_revision_id = Uuid::now_v7();
    let profile =
        QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() };
    let authoritative_evidence = AuthoritativeAnswerEvidence {
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
                normalized_value_text: "pageNumber".to_string(),
                normalized_value_exact: "pageNumber".to_string(),
                normalized_value_json: json!({ "value_type": "text", "value": "pageNumber" }),
                display_value: "pageNumber".to_string(),
                ..sample_technical_fact_row(
                    Uuid::now_v7(),
                    rewards_document_id,
                    rewards_revision_id,
                )
            },
            KnowledgeTechnicalFactRow {
                fact_kind: "url".to_string(),
                normalized_value_text: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                normalized_value_exact: "http://demo.local:8080/inventory-api/ws/inventory.wsdl"
                    .to_string(),
                normalized_value_json: json!(
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
    let preflight_evidence = build_preflight_authoritative_evidence(
        "What is the pageNumber parameter called in the pagination API?",
        &ir,
        &profile,
        &authoritative_evidence,
        &technical_literal_chunks,
    );

    let answer = build_evidence_preflight_answer(
        "What is the pageNumber parameter called in the pagination API?",
        &ir,
        &profile,
        None,
        &preflight_evidence,
        &preflight_chunks,
    )
    .expect("parameter preflight answer");

    assert!(answer.answer.contains("`pageNumber`"), "{answer:?}");
    assert!(!answer.answer.contains("inventory"), "{answer:?}");
}
