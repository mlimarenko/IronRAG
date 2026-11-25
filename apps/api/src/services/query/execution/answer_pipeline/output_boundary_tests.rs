use super::*;

#[test]
fn visible_prefix_of_a_long_entity_summary_supports_only_the_rendered_literal() {
    let mut prepared = prepared_for_single_shot(sample_ir(0.8, None));
    let node = super::super::types::RuntimeMatchedEntity {
        node_id: Uuid::from_u128(6),
        label: "hintOnlyField".into(),
        node_type: "service".into(),
        summary: Some(format!(
            "Configure `standbyTimeout`. {} Use `omittedField`.",
            "Наблюдение. ".repeat(80)
        )),
        score: None,
    };
    prepared.structured.graph_entity_references = vec![node];
    prepared.answer_context = super::super::context::assemble_bounded_context(
        &prepared.structured.graph_entity_references,
        &[],
        &[],
        4096,
    );
    assert!(prepared.answer_context.contains("standbyTimeout"));
    assert!(prepared.answer_context.contains("hintOnlyField"));
    assert!(!prepared.answer_context.contains("omittedField"));
    let grounding =
        selected_runtime_grounding_evidence(&prepared, AssistantGroundingEvidence::default());
    let verification = verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "Which fields are documented?",
        answer: "Set `standbyTimeout`, `omittedField` and `hintOnlyField`.",
        intent_profile: &prepared.structured.intent_profile,
        evidence: &super::super::AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: vec![],
            structured_blocks: vec![],
            technical_facts: vec![],
        },
        chunks: &[],
        assistant_grounding: &grounding,
    });
    assert!(!verification.unsupported_literals.iter().any(|s| s == "standbyTimeout"));
    for unsupported in ["omittedField", "hintOnlyField"] {
        assert!(
            verification.unsupported_literals.iter().any(|s| s == unsupported),
            "{unsupported}"
        );
    }
}

#[test]
fn literal_revision_and_verification_use_graph_statements_not_hints_or_the_prompt() {
    let mut prepared = prepared_for_single_shot(sample_ir(0.8, None));
    let edge = super::super::types::RuntimeMatchedRelationship {
        edge_id: Uuid::from_u128(3),
        from_node_id: Uuid::from_u128(4),
        to_node_id: Uuid::from_u128(5),
        from_label: "indexedField".into(),
        relation_type: "hintedRelation".into(),
        to_label: "otherEndpoint".into(),
        summary: Some("The recorded field is `recordedField`.".into()),
        support_count: 3,
        score: None,
    };
    prepared.structured.retrieved_documents[0].title = "titleOnlyField".into();
    prepared.structured.context_text = format!("Document: titleOnlyField\n{}", edge.context_line());
    prepared.answer_context =
        format!("Question: queryOnlyField?\n{}", prepared.structured.context_text);
    prepared.structured.graph_relation_references.push(edge);
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "[MCP tool result: grounded_answer]\ncontent:\ninvented claim".to_owned(),
        ],
        document_references: vec![
            crate::services::query::assistant_grounding::AssistantGroundingDocumentReference {
                document_id: Uuid::from_u128(1),
                revision_id: Some(Uuid::from_u128(2)),
                document_title: "not a passage".into(),
                source_uri: None,
                source_access: None,
                slice_start_offset: 0,
                slice_end_offset: "source passage".len(),
                excerpt: "source passage".into(),
                rank: 1,
            },
        ],
    };
    let stage = super::super::types::AnswerGenerationStage {
        intent_profile: Default::default(),
        primary_answer_chunks: selected_runtime_answer_chunks(&prepared),
        authoritative_evidence: super::super::AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: vec![],
            structured_blocks: vec![],
            technical_facts: vec![],
        },
        assistant_grounding: selected_runtime_grounding_evidence(&prepared, grounding),
        answer: String::new(),
        usage_json: serde_json::Value::Null,
        prompt_context: prepared.answer_context.clone(),
        query_ir: prepared.query_ir.clone(),
    };
    let context = output_boundary::literal_revision_context(&stage);
    let preflight = output_boundary::preflight_graph_grounding(&prepared, &stage.prompt_context);
    assert_eq!(
        preflight.source_texts().collect::<Vec<_>>(),
        ["The recorded field is `recordedField`."]
    );
    assert!(
        output_boundary::preflight_graph_grounding(&prepared, "Different document")
            .source_texts()
            .next()
            .is_none()
    );
    let verify = |answer| {
        verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
            question: "What field was recorded?",
            answer,
            intent_profile: &stage.intent_profile,
            chunks: &stage.primary_answer_chunks,
            evidence: &stage.authoritative_evidence,
            assistant_grounding: &stage.assistant_grounding,
        })
    };
    let unsupported =
        verify("Use `indexedField`, `hintedRelation`, `titleOnlyField` and `queryOnlyField`.")
            .unsupported_literals;
    for literal in ["indexedField", "hintedRelation", "titleOnlyField", "queryOnlyField"] {
        assert!(!context.contains(literal), "{literal}");
        assert!(unsupported.iter().any(|value| value == literal), "{literal}");
    }
    assert!(verify("Use `recordedField`.").unsupported_literals.is_empty());
    assert!(context.contains("The recorded field is `recordedField`."));
    assert!(context.contains("source passage"));
    assert!(!context.contains("not a passage"));
    assert!(!context.contains("invented claim"));
}

#[test]
fn strips_trailing_interrogative_meta_paragraph_after_numbered_inventory() {
    let answer = "1. Alpha item\n2. Beta item\n\nMore detail?";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
        Some("1. Alpha item\n2. Beta item")
    );
}

#[test]
fn strips_meta_paragraph_after_multiline_inventory_item() {
    let answer =
        "1. Alpha item\n   Source: alpha.md\n2. Beta item\n   Source: beta.md\n\nMore detail?";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
        Some("1. Alpha item\n   Source: alpha.md\n2. Beta item\n   Source: beta.md")
    );
}

#[test]
fn strips_short_meta_paragraph_when_requested_inventory_count_is_satisfied() {
    let answer = "1. Alpha item\n2. Beta item\n\nOverall theme: Alpha and Beta changes.";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, Some(2)).as_deref(),
        Some("1. Alpha item\n2. Beta item")
    );
}

#[test]
fn strips_trailing_interrogative_meta_paragraph_after_bulleted_inventory() {
    let answer = "- Alpha item\n- Beta item\n\nContinue？";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
        Some("- Alpha item\n- Beta item")
    );
}

#[test]
fn preserves_untyped_trailing_prose_in_non_latin_script() {
    let answer = "Сначала проверьте журнал обработки и исходную запись.\n\nЕсли хотите, я могу помочь найти исходную операцию.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_untyped_trailing_prose_in_latin_script() {
    let answer = "Validate the duplicate record before changing it.\n\nLet me know if you'd like a step-by-step checklist.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_substantive_russian_conditional_remediation() {
    let answer = "Сначала проверьте журнал обработки.\n\nЕсли ошибка повторяется, перезапустите сервисный модуль и снова проверьте журнал.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_substantive_russian_capability_statement() {
    let answer = "Проверка завершена.\n\nЯ могу подтвердить два изменения: новый формат и проверку входных данных.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_substantive_english_conditional_remediation() {
    let answer = "Validate the duplicate record.\n\nIf the error persists, restart the service and check the log again.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_trailing_coverage_limit_statement() {
    let answer = "1. Alpha item\n2. Beta item\n\nOnly the first two matching items were grounded.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_non_inventory_question_answer() {
    let answer = "The selected evidence contains two matching items. Which one is relevant?";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_trailing_technical_paragraph() {
    let answer = "1. Alpha item\n2. Beta item\n\nCheck `alpha --status`?";

    assert!(strip_trailing_inventory_meta_paragraph(answer, Some(2)).is_none());
}
