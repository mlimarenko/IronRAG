use super::*;

#[test]
fn disposition_prefers_query_specific_variant_titles_over_noise() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "Notification Console Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "WorkflowLink Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Embedded Browser Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:3".to_string()],
        },
        GroupedReference {
            id: "document:4".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 4,
            title: "WorkflowLink Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:4".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "how configure workflowlink?",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &[],
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(
                variants,
                vec![
                    "WorkflowLink Subject Alpha Manual".to_string(),
                    "WorkflowLink Subject Beta Manual".to_string(),
                ]
            );
        }
        AnswerDisposition::Answer => {
            panic!("expected clarify disposition with query-aligned variants")
        }
    }
}

#[test]
fn disposition_uses_discriminating_topic_token_over_shared_product_tokens() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "Platform Pay Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "Platform Inventory Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Platform Pay Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "platform pay",
        &sample_ir_with_act(
            QueryAct::RetrieveValue,
            0.4,
            Some(ClarificationReason::AmbiguousTooShort),
        ),
        &[],
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(
                variants,
                vec![
                    "Platform Pay Subject Alpha Manual".to_string(),
                    "Platform Pay Subject Beta Manual".to_string(),
                ]
            );
        }
        AnswerDisposition::Answer => {
            panic!("expected discriminating topic token to filter shared product labels")
        }
    }
}

#[test]
fn disposition_uses_query_specific_retrieved_documents_when_group_titles_are_noisy() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "Notification Console Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "Embedded Browser Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Inventory Exchange Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];
    let retrieved_documents = vec![
        crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
            title: "WorkflowLink Subject Alpha Manual".to_string(),
            preview_excerpt: String::new(),
            document_hint: None,
        },
        crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
            title: "WorkflowLink Subject Beta Manual".to_string(),
            preview_excerpt: String::new(),
            document_hint: None,
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "how configure workflowlink?",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &retrieved_documents,
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(
                variants,
                vec![
                    "WorkflowLink Subject Alpha Manual".to_string(),
                    "WorkflowLink Subject Beta Manual".to_string(),
                ]
            );
        }
        AnswerDisposition::Answer => {
            panic!("expected clarify disposition with query-aligned retrieved documents")
        }
    }
}

#[test]
fn disposition_uses_final_context_titles_when_briefs_and_groups_are_truncated() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "WorkflowLink Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 6,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "WorkflowLink Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:2".to_string()],
        },
    ];
    let context_titles = vec![
        "WorkflowLink Subject Alpha Manual".to_string(),
        "WorkflowLink Subject Beta Manual".to_string(),
        "WorkflowLink Subject Gamma Manual".to_string(),
        "WorkflowLink Provider Delta Manual".to_string(),
    ];

    let disposition = classify_answer_disposition_from_evidence(
        "how configure workflowlink?",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &[],
        &context_titles,
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(variants, context_titles);
        }
        AnswerDisposition::Answer => {
            panic!("expected final context titles to preserve the variant menu")
        }
    }
}

#[test]
fn disposition_answers_when_only_one_focused_document_is_supported() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "Container Return Procedure".to_string(),
            excerpt: None,
            evidence_count: 4,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "node:1".to_string(),
            kind: GroupedReferenceKind::Entity,
            rank: 2,
            title: "return document".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["node:1".to_string()],
        },
    ];
    let retrieved_documents =
        vec![crate::services::query::execution::types::RuntimeRetrievedDocumentBrief {
            title: "Container Return Procedure".to_string(),
            preview_excerpt: String::new(),
            document_hint: None,
        }];
    let context_titles = vec!["Container Return Procedure".to_string()];

    let disposition = classify_answer_disposition_from_evidence(
        "how do i process container return?",
        &sample_ir(0.4, Some(ClarificationReason::MultipleInterpretations)),
        &retrieved_documents,
        &context_titles,
        &groups,
    );

    assert!(
        matches!(disposition, AnswerDisposition::Answer),
        "one supported candidate must not produce an empty clarification"
    );
}

#[test]
fn disposition_answers_when_only_one_query_aligned_variant_survives() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "TargetName Workflow Connector Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "Inventory Exchange Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Embedded Browser Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "targetnme how",
        &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
        &[],
        &groups,
    );

    assert!(
        matches!(disposition, AnswerDisposition::Answer),
        "one query-aligned variant must not produce an empty clarification"
    );
}

#[test]
fn disposition_answers_when_evidence_has_no_query_aligned_variants() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "Inventory Exchange Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "Embedded Browser Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Notification Console Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "targetname how",
        &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
        &[],
        &groups,
    );

    assert!(
        matches!(disposition, AnswerDisposition::Answer),
        "unmatched labels must not produce an empty or invented clarification menu"
    );
}

#[test]
fn disposition_answers_when_only_substring_matches_exist() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "Branch Director".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "Fruit Notes".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Operations Handbook".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "who is TargetName?",
        &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
        &[],
        &groups,
    );

    assert!(
        matches!(disposition, AnswerDisposition::Answer),
        "question-word substrings must not create an empty clarification"
    );
}

#[test]
fn disposition_keeps_short_acronym_variants_on_exact_token_match() {
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
            title: "Notification Console Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "API Gateway Beta".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "how configure api",
        &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
        &[],
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(
                variants,
                vec!["API Gateway Alpha".to_string(), "API Gateway Beta".to_string()]
            );
        }
        AnswerDisposition::Answer => {
            panic!("expected exact short acronym matches to remain valid variants")
        }
    }
}

#[test]
fn disposition_clarifies_with_multiple_fuzzy_query_specific_variants() {
    let groups = vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "TargetName Subject Alpha Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "Notification Console Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:2".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "TargetName Subject Beta Manual".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:3".to_string()],
        },
    ];

    let disposition = classify_answer_disposition_from_groups(
        "targetnme how",
        &sample_ir(0.4, Some(ClarificationReason::AmbiguousTooShort)),
        &[],
        &groups,
    );

    match disposition {
        AnswerDisposition::Clarify { variants } => {
            assert_eq!(
                variants,
                vec![
                    "TargetName Subject Alpha Manual".to_string(),
                    "TargetName Subject Beta Manual".to_string(),
                ]
            );
        }
        AnswerDisposition::Answer => {
            panic!("expected clarify disposition with multiple fuzzy topic matches")
        }
    }
}

// ---------------------------------------------------------------------------
// QueryClarification builder unit tests
// ---------------------------------------------------------------------------

#[test]
fn disposition_clarification_emits_candidates_for_each_variant() {
    let question = "Which component do you mean?";
    let variants = vec!["Sample Subject".to_string(), "Subject Beta".to_string()];
    let clar = disposition_clarification(question, &variants);

    assert!(clar.required);
    assert_eq!(clar.question.as_deref(), Some(question));
    assert_eq!(clar.answer_candidates.len(), 2);

    let first = &clar.answer_candidates[0];
    assert_eq!(first.label, "Sample Subject");
    assert_eq!(first.kind, "document");
    assert!(first.confidence.is_none());
    assert!(first.provenance.entity_id.is_none());
    assert!(first.provenance.document_id.is_none());
    assert!(first.provenance.chunk_id.is_none());
}

#[test]
fn disposition_clarification_empty_variants_gives_no_candidates() {
    let clar = disposition_clarification("Which one?", &[]);
    assert!(clar.required);
    assert!(clar.answer_candidates.is_empty());
}
