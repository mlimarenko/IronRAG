use super::*;

#[test]
fn disposition_keeps_confident_ir_on_answer_path() {
    let disposition = classify_answer_disposition_from_groups(
        "how do i configure target workflows?",
        &sample_ir(0.9, None),
        &[],
        &sample_groups(),
    );

    assert!(matches!(disposition, AnswerDisposition::Answer));
}

#[test]
fn disposition_keeps_low_confidence_ir_on_answer_path_without_explicit_reason() {
    let disposition = classify_answer_disposition_from_groups(
        "how do i configure target workflows?",
        &sample_ir(0.4, None),
        &[],
        &sample_groups(),
    );

    assert!(matches!(disposition, AnswerDisposition::Answer));
}

#[test]
fn provider_free_fallback_ir_preserves_retrieval_question_without_clarifying() {
    let ir = provider_free_fallback_query_ir("  Describe ABC ConnectorX with `/v2/pay`  ");

    assert_eq!(ir.act, QueryAct::Describe);
    assert_eq!(ir.scope, QueryScope::MultiDocument);
    assert_eq!(ir.language, QueryLanguage::Auto);
    assert_eq!(ir.effective_retrieval_query("unused"), "Describe ABC ConnectorX with `/v2/pay`");
    assert!(ir.target_entities.is_empty());
    assert!(ir.literal_constraints.iter().any(|literal| literal.text == "/v2/pay"));
    assert!(ir.needs_clarification.is_none());
    assert!(ir.confidence < 0.5);
}

#[test]
fn provider_free_fallback_ir_stays_on_answer_path_with_retrieved_evidence() {
    let ir = provider_free_fallback_query_ir("describe S");
    let disposition =
        classify_answer_disposition_from_groups("describe S", &ir, &[], &sample_groups());

    assert!(matches!(disposition, AnswerDisposition::Answer));
}

#[test]
fn disposition_keeps_terse_low_confidence_query_on_answer_path_without_typed_reason() {
    let disposition = classify_answer_disposition_from_groups(
        "provider configure",
        &sample_ir_with_act(QueryAct::Describe, 0.25, None),
        &[],
        &sample_groups(),
    );

    assert!(matches!(disposition, AnswerDisposition::Answer));
}

#[test]
fn disposition_can_clarify_when_compiler_explicitly_requests_it() {
    let disposition = classify_answer_disposition_from_groups(
        "how do i configure provider workflows?",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &[],
        &sample_groups(),
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(variants.len(), 3);
            assert_eq!(variants[0], "Provider A configuration");
        }
        AnswerDisposition::Answer => {
            panic!("expected clarify disposition for explicit compiler clarification")
        }
    }
}

#[test]
fn disposition_honors_typed_clarification_for_technical_target() {
    let mut ir = sample_ir_with_act(
        QueryAct::RetrieveValue,
        0.4,
        Some(ClarificationReason::MultipleInterpretations),
    );
    ir.target_types = vec![QueryTargetKind::Endpoint];

    let disposition = classify_answer_disposition_from_groups(
        "which provider endpoint handles workflow module status?",
        &ir,
        &[],
        &sample_groups(),
    );

    assert!(matches!(disposition, AnswerDisposition::Clarify { .. }));
}

#[test]
fn disposition_honors_typed_clarification_for_non_terse_retrieve_value() {
    let mut ir = sample_ir_with_act(
        QueryAct::RetrieveValue,
        0.4,
        Some(ClarificationReason::MultipleInterpretations),
    );
    ir.target_types = vec![QueryTargetKind::Attribute];

    let disposition = classify_answer_disposition_from_groups(
        "which provider configuration owns the workflow module state?",
        &ir,
        &[],
        &sample_groups(),
    );

    assert!(matches!(disposition, AnswerDisposition::Clarify { .. }));
}

#[test]
fn clarify_variant_dedup_key_collapses_same_page_attachments() {
    // Several image attachments hanging off the same parent page share a
    // logical prefix and differ only in the trailing filename qualifier.
    // They must collapse to one dedup key so they cannot pad a clarify
    // menu with near-duplicate "variants".
    let a = clarify_variant_dedup_key("Workflow setup: screen-a.png");
    let b = clarify_variant_dedup_key("Workflow setup: screen-b.png");
    assert_eq!(a, b);
    assert_eq!(a.as_deref(), Some("workflow setup"));
}

#[test]
fn clarify_variant_dedup_key_excludes_bare_attachment_titles() {
    // A title that is only a filename is an attachment artefact with no
    // logical document behind it — exclude it from the menu entirely.
    assert_eq!(clarify_variant_dedup_key("diagram.png"), None);
    assert_eq!(clarify_variant_dedup_key("v2-config.jpeg"), None);
}

#[test]
fn clarify_variant_dedup_key_keeps_ordinary_titles() {
    // A normal prose title (no trailing filename qualifier) is a distinct
    // logical document and keeps its own dedup key. A title that merely
    // ends with a filename-shaped word but has no `:` separator is prose,
    // not an attachment qualifier, and is preserved verbatim.
    assert_eq!(
        clarify_variant_dedup_key("Subject Alpha configuration"),
        Some("subject alpha configuration".to_string())
    );
    assert_eq!(
        clarify_variant_dedup_key("How to edit config.ini"),
        Some("how to edit config.ini".to_string())
    );
}

#[test]
fn query_specific_variants_keep_all_matching_provider_titles() {
    let documents = vec![
        retrieved_doc("Subject Subject Alpha", "alpha"),
        retrieved_doc("Subject Subject Beta", "beta"),
        retrieved_doc("Subject Subject Gamma", "gamma"),
        retrieved_doc("Subject Provider Delta", "delta"),
    ];

    let variants =
        extract_query_specific_variants("how to configure Subject", &documents, &[], &[]);

    assert_eq!(
        variants,
        vec![
            "Subject Subject Alpha",
            "Subject Subject Beta",
            "Subject Subject Gamma",
            "Subject Provider Delta"
        ]
    );
}

#[test]
fn extract_variants_collapses_same_page_image_attachments() {
    // Five same-page image attachments share a title prefix; after the
    // structural collapse only one logical variant survives.
    let documents = vec![
        retrieved_doc("Workflow setup: a.png", "page#1"),
        retrieved_doc("Workflow setup: b.png", "page#1"),
        retrieved_doc("Workflow setup: c.png", "page#1"),
        retrieved_doc("Workflow setup: d.png", "page#1"),
        retrieved_doc("Workflow setup: e.png", "page#1"),
    ];

    let variants = extract_query_specific_variants("workflow setup", &documents, &[], &[]);

    assert!(
        variants.len() <= 1,
        "same-page image attachments must collapse to at most one variant, got {variants:?}"
    );
}

#[test]
fn disposition_answers_for_same_page_image_attachments() {
    // (a) N same-page image-attachment documents sharing a title prefix
    // must NOT manufacture a clarify menu of near-duplicates; they
    // collapse below the distinct-document floor and fall through to
    // Answer.
    let groups = (0..5)
        .map(|index| GroupedReference {
            id: format!("document:{index}"),
            kind: GroupedReferenceKind::Document,
            rank: index + 1,
            title: format!("Workflow setup: attachment-{index}.png"),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec![format!("chunk:{index}")],
        })
        .collect::<Vec<_>>();

    let disposition = classify_answer_disposition_from_groups(
        "workflow setup",
        &sample_ir_with_act(QueryAct::ConfigureHow, 0.3, None),
        &[],
        &groups,
    );

    assert!(
        matches!(disposition, AnswerDisposition::Answer),
        "near-duplicate image attachments must answer, not clarify"
    );
}

#[test]
fn disposition_does_not_cancel_typed_clarification_from_evidence_counts() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "TargetName Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 9,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "TargetName Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Ancillary Reference Guide".to_string(),
            excerpt: None,
            evidence_count: 4,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "targetname configure",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &[],
        &groups,
    );

    assert!(matches!(disposition, AnswerDisposition::Clarify { .. }));
}

#[test]
fn disposition_clarifies_with_two_variants_when_top_two_are_balanced() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "TargetName Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 8,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "TargetName Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 7,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Ancillary Reference Guide".to_string(),
            excerpt: None,
            evidence_count: 4,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "targetname configure",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &[],
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(
                variants,
                vec![
                    "TargetName Subject Alpha Manual".to_string(),
                    "TargetName Subject Beta Manual".to_string()
                ]
            );
        }
        AnswerDisposition::Answer => {
            panic!("expected clarification when two variants are competitively balanced")
        }
    }
}

#[test]
fn disposition_clarifies_two_variants_without_absolute_evidence_gap() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "TargetName Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "TargetName Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 1,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Ancillary Reference Guide".to_string(),
            excerpt: None,
            evidence_count: 1,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "targetname configure",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &[],
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(
                variants,
                vec![
                    "TargetName Subject Alpha Manual".to_string(),
                    "TargetName Subject Beta Manual".to_string()
                ]
            );
        }
        AnswerDisposition::Answer => {
            panic!("compiler clarification should not answer on a weak 2:1 evidence split")
        }
    }
}

#[test]
fn disposition_allows_explicit_compiler_clarify_with_multiple_target_entities() {
    let ir = sample_ir_with_two_target_entities(
        QueryAct::RetrieveValue,
        0.4,
        Some(ClarificationReason::AmbiguousTooShort),
    );

    let disposition = classify_answer_disposition_from_groups(
        "provider configuration",
        &ir,
        &[],
        &sample_groups(),
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(variants.len(), 3);
            assert_eq!(variants[0], "Provider A configuration");
        }
        AnswerDisposition::Answer => {
            panic!("explicit compiler clarification must bypass multi-entity specificity")
        }
    }
}

#[test]
fn disposition_does_not_clarify_configure_without_typed_reason() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "WorkflowLink Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "WorkflowLink Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "WorkflowLink Subject Gamma Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "how configure workflowlink?",
        &sample_ir(0.6, None),
        &[],
        &groups,
    );

    assert!(matches!(disposition, AnswerDisposition::Answer));
}

#[test]
fn disposition_answers_non_terse_configure_without_compiler_reason() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "WorkflowLink Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "WorkflowLink Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "WorkflowLink Subject Gamma Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "how should operators configure workflowlink provider routing before rollout?",
        &sample_ir(0.6, None),
        &[],
        &groups,
    );

    assert!(
        matches!(disposition, AnswerDisposition::Answer),
        "non-terse configure questions should answer from retrieved evidence unless the compiler requested clarification"
    );
}

#[test]
fn disposition_does_not_clarify_terse_followup_without_typed_reason() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "WorkflowLink Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "WorkflowLink Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "WorkflowLink Subject Gamma Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "workflowlink platform",
        &sample_ir_with_two_target_entities(QueryAct::RetrieveValue, 0.6, None),
        &[],
        &groups,
    );

    assert!(matches!(disposition, AnswerDisposition::Answer));
}

#[test]
fn disposition_does_not_structurally_clarify_describe_without_compiler_reason() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "API Gateway Alpha".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "API Gateway Beta".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "API Gateway Gamma".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "what is api?",
        &sample_ir_with_act(QueryAct::Describe, 0.6, None),
        &[],
        &groups,
    );

    assert!(matches!(disposition, AnswerDisposition::Answer));
}
