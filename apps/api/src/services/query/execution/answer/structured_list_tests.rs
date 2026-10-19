use crate::domains::query_ir::LiteralSpan;

use super::test_support::*;
use super::*;

#[test]
fn deterministic_answer_yields_troubleshooting_lists_to_grounded_synthesis() {
    let mut query_ir = configure_update_focus_ir("Sample Incident");
    query_ir.target_types = vec![
        QueryTargetKind::Procedure,
        QueryTargetKind::Troubleshooting,
        QueryTargetKind::Remediation,
        QueryTargetKind::ErrorMessage,
    ];
    query_ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "sample operation was already completed".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Other,
    }];
    let mut generic_runbook = evidence_chunk(
        1,
        Some("paragraph"),
        "Error: sample operation was already completed.\n\
             Sample Incident workflow:\n\
             1. Open the generic operations screen.\n\
             2. Review the current queue.\n\
             3. Continue the ordinary workflow.",
    );
    generic_runbook.document_label = "Sample operations guide".to_string();
    let evidence = AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };

    let answer = build_deterministic_grounded_answer(
        "What should I do when the error says \"sample operation was already completed\"?",
        &query_ir,
        &evidence,
        &[generic_runbook],
    );

    assert!(
        answer.is_none(),
        "typed troubleshooting must reach grounded remediation synthesis instead of any deterministic list shortcut: {answer:?}"
    );
}

#[test]
fn structured_list_answer_extracts_supported_bullets_from_evidence() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.retrieval_query =
        Some("supported alpha beta gamma item types vector mode".to_string());
    let chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Overview\n\nSupported Items:\n- Alpha + Beta mode\n- Gamma mode\n- Delta lightweight mode\n\nMetrics:\n- Noise\n- Distractor",
    );

    let answer = build_structured_list_grounded_answer(
        "What item types does the component support, and which use vector mode?",
        &query_ir,
        &[chunk],
    )
    .expect("supported item list answer");

    assert!(answer.contains("Alpha + Beta mode"), "{answer}");
    assert!(answer.contains("Gamma mode"), "{answer}");
    assert!(answer.contains("Delta lightweight mode"), "{answer}");
    assert!(!answer.contains("Noise"), "{answer}");
}

#[test]
fn structured_list_answer_preserves_enum_variant_transition_targets() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.scope = QueryScope::SingleDocument;
    query_ir.target_types = vec![QueryTargetKind::State];
    query_ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Identifier, text: "Queued".to_string() }];
    query_ir.retrieval_query =
        Some("FlowStage Queued Running Aborted transition targets".to_string());
    let chunk = evidence_chunk(
        1,
        Some("code_block"),
        "impl FlowStage {\n\
                 fn next(&self) -> &[FlowStage] {\n\
                     match self {\n\
                         FlowStage::Queued => &[FlowStage::Running, FlowStage::Aborted],\n\
                         FlowStage::Running => &[FlowStage::Finished],\n\
                         FlowStage::Finished | FlowStage::Aborted => &[],\n\
                     }\n\
                 }\n\
             }",
    );

    let answer = build_structured_list_grounded_answer(
        "Which transitions are valid from Queued?",
        &query_ir,
        &[chunk],
    )
    .expect("structural transition answer");

    assert_eq!(answer, "`FlowStage::Queued`:\n- `FlowStage::Running`\n- `FlowStage::Aborted`");
}

#[test]
fn structured_list_answer_rejects_empty_focus_instead_of_selecting_the_largest_list() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    let chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Architecture decisions:\n- Unrelated first item\n- Unrelated second item\n- Unrelated third item",
    );

    let answer = build_structured_list_grounded_answer("Opaque enumeration", &query_ir, &[chunk]);

    assert!(
        answer.is_none(),
        "typed admission without structural focus must fall through to synthesis: {answer:?}"
    );
}

#[test]
fn comment_list_requires_a_word_boundary_focus_anchor() {
    let focus_terms = BTreeSet::from(["node".to_string()]);
    let lines = [
        "# Runtime configuration",
        "# NODE_ENV selects the deployment mode",
        "# PORT selects the listener",
    ];

    assert!(
        extract_comment_list_candidates(&lines, &focus_terms, 0).is_empty(),
        "an identifier fragment must not anchor a comment list"
    );
}

#[test]
fn comment_list_rejects_non_contiguous_blocks_instead_of_fusing_them() {
    let focus_terms = BTreeSet::from(["cloudwatch".to_string()]);
    let lines = [
        "# CloudWatch alarms",
        "# CPU threshold",
        "resource block separates comments",
        "# Memory threshold",
        "# Error threshold",
    ];

    assert!(
        extract_comment_list_candidates(&lines, &focus_terms, 0).is_empty(),
        "separate comment blocks must fall through rather than form a new list"
    );
}

#[test]
fn structured_list_comment_path_keeps_cloudwatch_alarm_guard_case() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.retrieval_query =
        Some("Terraform CloudWatch alarms CPU memory connections 5xx".to_string());
    let chunk = evidence_chunk(
        1,
        Some("code_block"),
        "# Complete AWS infrastructure with Terraform.\n\
             #\n\
             # Architecture decisions:\n\
             # - CloudWatch alarms for CPU thresholds.\n\
             # - CloudWatch alarms for memory thresholds.\n\
             # - CloudWatch alarms for connection and 5xx thresholds.",
    );

    let answer = build_structured_list_grounded_answer(
        "What CloudWatch alarms does the Terraform configuration define?",
        &query_ir,
        &[chunk],
    )
    .expect("the anchored contiguous CloudWatch comment list remains deterministic");

    assert!(answer.contains("CloudWatch alarms for CPU thresholds"), "{answer}");
    assert!(answer.contains("CloudWatch alarms for memory thresholds"), "{answer}");
}

#[test]
fn structured_list_answer_defers_to_synthesis_instead_of_truncating_over_render_cap() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.retrieval_query = Some("supported item types".to_string());
    let items = (1..=17).map(|index| format!("- Supported item {index:02}"));
    let source_text = std::iter::once("Supported item types:".to_string())
        .chain(items)
        .collect::<Vec<_>>()
        .join("\n");
    let chunk = evidence_chunk(1, Some("paragraph"), &source_text);

    let answer = build_structured_list_grounded_answer(
        "Which supported item types are available?",
        &query_ir,
        &[chunk],
    );

    assert!(
        answer.is_none(),
        "a deterministic list larger than the render cap must defer to synthesis: {answer:?}"
    );
}

#[test]
fn structured_list_answer_uses_typed_ir_without_question_keywords() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "DeltaProcessor".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir.retrieval_query = Some("DeltaProcessor alpha beta gamma".to_string());
    let chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "DeltaProcessor:\n- Alpha branch\n- Beta branch\n- Gamma branch",
    );

    let answer =
        build_structured_list_grounded_answer("Δelta?", &query_ir, &[chunk]).expect("list answer");

    assert!(answer.contains("Alpha branch"), "{answer}");
    assert!(answer.contains("Beta branch"), "{answer}");
    assert!(answer.contains("Gamma branch"), "{answer}");
}

#[test]
fn structured_list_answer_abstains_for_typed_table_column_inventory() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "accounts".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir.retrieval_query = Some("Which columns are in the accounts table?".to_string());
    let chunk = evidence_chunk(
        1,
        Some("code_block"),
        "// Application route handlers\n// Accounts list endpoint\n// Handler queries records",
    );

    assert!(
        build_structured_list_grounded_answer(
            "Which columns are in the accounts table?",
            &query_ir,
            &[chunk],
        )
        .is_none()
    );
}

#[test]
fn structured_list_answer_extracts_ordered_steps_from_evidence() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.retrieval_query =
        Some("processor steps lowercase html url email whitespace".to_string());
    let chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "class TextProcessor:\n    \"\"\"Applies the following transformations in order:\n    1. Lowercase conversion\n    2. HTML tag removal\n    3. URL replacement\n    4. Email replacement\n    5. Whitespace normalization\n    \"\"\"",
    );

    let answer = build_structured_list_grounded_answer(
        "What processing steps does TextProcessor apply, and in what order?",
        &query_ir,
        &[chunk],
    )
    .expect("ordered list answer");

    assert!(answer.contains("1. Lowercase conversion"), "{answer}");
    assert!(answer.contains("2. HTML tag removal"), "{answer}");
    assert!(answer.contains("3. URL replacement"), "{answer}");
    assert!(answer.contains("4. Email replacement"), "{answer}");
    assert!(answer.contains("5. Whitespace normalization"), "{answer}");
}

#[test]
fn structured_list_answer_yields_non_actionable_how_to_inventory_to_synthesis() {
    let mut query_ir = configure_how_focus_ir("Sample Connector");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Connector configuration:\n\
             1. [Main]\n\
             2. endpointUrl\n\
             3. Default value false",
    );
    chunk.document_label = "Sample Connector setup guide".to_string();

    let answer = build_structured_list_grounded_answer(
        "How do I configure Sample Connector?",
        &query_ir,
        &[chunk],
    );

    assert!(
        answer.is_none(),
        "a how-to inventory without two action-bearing steps must reach grounded synthesis: {answer:?}"
    );
}

#[test]
fn structured_list_answer_keeps_actionable_how_to_steps() {
    let mut query_ir = configure_how_focus_ir("Sample Connector");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Connector setup:\n\
             1. Install the connector package.\n\
             2. Open the connector settings.\n\
             3. Save the configured mode.",
    );
    chunk.document_label = "Sample Connector setup guide".to_string();

    let answer = build_structured_list_grounded_answer(
        "How do I configure Sample Connector?",
        &query_ir,
        &[chunk],
    )
    .expect("actionable structured procedure");

    assert!(answer.contains("Install the connector package"), "{answer}");
    assert!(answer.contains("Open the connector settings"), "{answer}");
}

#[test]
fn deterministic_answer_labels_follow_explicit_query_language() {
    let english = deterministic_answer_labels(&configure_update_focus_ir("Sample Target"));
    assert_eq!(english.update_sequence, "Steps");
    assert_eq!(english.parameter_details, "Parameter details");

    let mut russian_ir = configure_update_focus_ir("S1");
    russian_ir.language = crate::domains::query_ir::QueryLanguage::Ru;
    let russian = deterministic_answer_labels(&russian_ir);
    assert_eq!(russian.parameter_details, i18n::RU_DETERMINISTIC_ANSWER_LABELS.parameter_details);

    let auto_question = deterministic_answer_labels(&configure_update_focus_ir("S1"));
    assert_eq!(auto_question.update_sequence, "Steps");
}

#[test]
fn structured_list_extraction_stays_inside_the_focused_document() {
    let mut ir = crate::domains::query_ir::QueryIR::test_with_act_scope(
        crate::domains::query_ir::QueryAct::Enumerate,
        crate::domains::query_ir::QueryScope::SingleDocument,
    );
    // An unfocused low-confidence enumeration is exactly the shape that
    // let the strongest foreign list win before extraction was bound to
    // the asked document.
    ir.confidence = 0.5;
    ir.retrieval_query = Some("environment variables ASKED_PRIMARY_URL ASKED_TIMEOUT".to_string());
    let asked_document = Uuid::now_v7();
    let mut asked_chunk =
        evidence_chunk(0, None, "Environment variables:\n- ASKED_PRIMARY_URL\n- ASKED_TIMEOUT");
    asked_chunk.document_id = asked_document;
    asked_chunk.document_label = "asked_service_config.md".to_string();
    asked_chunk.score = Some(0.4);
    let mut other_chunk =
        evidence_chunk(0, None, "Environment variables:\n- OTHER_PRIMARY_URL\n- OTHER_TIMEOUT");
    other_chunk.document_id = Uuid::now_v7();
    other_chunk.document_label = "other_service_config.md".to_string();
    other_chunk.score = Some(0.99);

    let answer = build_structured_list_grounded_answer(
        "Which environment variables does asked_service_config.md define?",
        &ir,
        &[asked_chunk, other_chunk],
    );

    let answer = answer.expect("the focused document has a list candidate");
    assert!(answer.contains("ASKED_PRIMARY_URL"), "{answer}");
    assert!(
        !answer.contains("OTHER_PRIMARY_URL"),
        "a higher-scoring foreign document must not answer this question: {answer}"
    );
}

#[test]
fn structured_list_question_fit_requires_item_scoped_word_boundary_matching() {
    let ir = crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::Enumerate,
        scope: crate::domains::query_ir::QueryScope::SingleDocument,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: vec![crate::domains::query_ir::LiteralSpan {
            text: "node".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        needs_clarification: None,
        conversation_refs: Vec::new(),
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };
    let fragment_candidate = StructuredListCandidate {
        items: vec!["NODE_ENV configuration".to_string(), "PORT configuration".to_string()],
        ordered: true,
        score: 8,
        first_chunk_index: 0,
    };
    let source_document = Uuid::now_v7();
    let mut source_chunk = evidence_chunk(0, None, "The node groups are documented here.");
    source_chunk.document_id = source_document;
    let chunks = vec![source_chunk];

    assert!(
        !structured_list_source_addresses_question_focus(&ir, &fragment_candidate, &chunks),
        "source-document coverage and an identifier fragment must not vouch for the items"
    );

    let anchored_candidate = StructuredListCandidate {
        items: vec!["Application node group".to_string(), "System node group".to_string()],
        ordered: true,
        score: 8,
        first_chunk_index: 0,
    };
    assert!(structured_list_source_addresses_question_focus(&ir, &anchored_candidate, &chunks,));
}

#[test]
fn structured_list_question_fit_selects_the_least_document_frequency_probe_tier() {
    let ir = crate::domains::query_ir::QueryIR {
        act: crate::domains::query_ir::QueryAct::Enumerate,
        scope: crate::domains::query_ir::QueryScope::MultiDocument,
        language: crate::domains::query_ir::QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        needs_clarification: None,
        conversation_refs: Vec::new(),
        source_slice: None,
        retrieval_query: Some("inventory api".to_string()),
        confidence: 0.9,
    };
    let generic_candidate = StructuredListCandidate {
        items: vec!["API setup".to_string(), "API validation".to_string()],
        ordered: true,
        score: 8,
        first_chunk_index: 0,
    };
    let inventory_candidate = StructuredListCandidate {
        items: vec!["Inventory event".to_string(), "Inventory signature".to_string()],
        ordered: true,
        score: 8,
        first_chunk_index: 0,
    };
    let chunks = vec![
        evidence_chunk(0, None, "Inventory API event catalog"),
        evidence_chunk(1, None, "Generic API reference"),
    ];

    assert!(
        !structured_list_source_addresses_question_focus(&ir, &generic_candidate, &chunks),
        "the bundle-wide API term must not outrank the one-document inventory term"
    );
    assert!(structured_list_source_addresses_question_focus(&ir, &inventory_candidate, &chunks,));
}
