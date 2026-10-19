use super::test_prelude::*;

#[test]
fn setup_focus_candidates_match_focused_title_without_unique_document_target() {
    let target = document_row("target.md", "Subject Alpha setup manual");
    let screenshot = document_row("image.png", "Subject Alpha: screenshot");
    let unrelated = document_row("other.md", "Environment Variant setup manual");
    let document_index = HashMap::from([
        (target.document_id, target.clone()),
        (screenshot.document_id, screenshot.clone()),
        (unrelated.document_id, unrelated),
    ]);
    let mut query_ir = setup_query_ir("Subject Alpha");

    let candidates = setup_focus_candidate_document_ids(&query_ir, &document_index, 8);

    assert_eq!(candidates, [target.document_id]);

    query_ir.source_slice = Some(SourceSliceSpec {
        direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        count: None,
        filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
    });
    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn setup_focus_candidates_prioritize_entity_focus_before_generic_document_focus() {
    let focused =
        document_row("environment-beta-admin-guide.md", "Environment Variant setup reference");
    let generic = document_row("alpha-suite-admin-guide.md", "Sample Subject administration");
    let screenshot = document_row("environment-beta.png", "Environment Variant subject");
    let document_index = HashMap::from([
        (focused.document_id, focused.clone()),
        (generic.document_id, generic.clone()),
        (screenshot.document_id, screenshot),
    ]);
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.target_entities =
        vec![EntityMention { label: "Environment Variant".to_string(), role: EntityRole::Object }];

    let candidates = setup_focus_candidate_document_ids(&query_ir, &document_index, 8);

    assert_eq!(candidates.first(), Some(&focused.document_id));
    assert!(candidates.contains(&generic.document_id));
}

#[test]
fn configure_procedure_queries_with_document_focus_request_setup_focus_candidates() {
    // A configure/how-to question that points at one specific document via
    // document_focus must engage the focused-document lane even when the
    // compiler tagged the target only as a generic procedure — otherwise the
    // focused document's own install/config chunks get diluted by a
    // multi-document salad.
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.target_types = vec![QueryTargetKind::Procedure];

    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn configure_procedure_queries_without_focus_or_setup_target_skip_setup_focus() {
    // Without an explicit document_focus and without a command-object/configuration target
    // type, a bare procedure-tagged configure query stays out of the
    // focused-document lane so broad how-to questions are not over-scoped.
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.target_types = vec![QueryTargetKind::Procedure];
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "settings".to_string(), role: EntityRole::Object }];

    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn configure_subject_queries_without_setup_target_request_setup_focus_candidates() {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];

    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    assert_eq!(setup_focus_query_identity_terms(&query_ir), ["Sample Subject".to_string()]);
}

#[test]
fn configure_subject_setup_focus_skips_ambiguous_subject_context() {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
    query_ir.document_focus = None;
    query_ir.target_entities = vec![
        EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject },
        EntityMention { label: "Beta Tool".to_string(), role: EntityRole::Subject },
    ];
    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));

    query_ir.target_entities =
        vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];
    query_ir.comparison = Some(crate::domains::query_ir::ComparisonSpec {
        a: None,
        b: None,
        dimension: "compatibility".to_string(),
    });
    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));

    query_ir.comparison = None;
    query_ir.literal_constraints =
        vec![LiteralSpan { text: "1.2.3".to_string(), kind: LiteralKind::Version }];
    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn setup_focus_candidates_use_target_entities_without_document_focus() {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];

    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    assert_eq!(setup_focus_query_identity_terms(&query_ir), vec!["Sample Subject".to_string()]);
}

#[test]
fn setup_focus_candidates_match_short_acronym_subject_exactly() {
    let target = document_row("target.md", "QX setup manual");
    let unrelated = document_row("other.md", "QY setup manual");
    let document_index =
        HashMap::from([(target.document_id, target.clone()), (unrelated.document_id, unrelated)]);
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "QX".to_string(), role: EntityRole::Subject }];

    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    assert_eq!(
        setup_focus_candidate_document_ids(&query_ir, &document_index, 8),
        [target.document_id]
    );

    query_ir.target_entities =
        vec![EntityMention { label: "qx".to_string(), role: EntityRole::Subject }];
    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn setup_focus_acronym_identity_is_unicode_case_aware() {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "абв".to_string(), role: EntityRole::Subject }];

    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));

    query_ir.target_entities =
        vec![EntityMention { label: "АБВ".to_string(), role: EntityRole::Subject }];
    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn setup_focus_candidates_ignore_single_token_entity_without_document_focus() {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "settings".to_string(), role: EntityRole::Object }];

    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn setup_focus_candidates_ignore_object_entity_without_document_focus() {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "retry timeout".to_string(), role: EntityRole::Object }];

    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}

#[test]
fn configure_followup_object_identity_requests_setup_focus_candidates() {
    let target = document_row("delta-pay.html", "DeltaVariant setup reference");
    let unrelated = document_row("alpha-subject.html", "AlphaSubject setup reference");
    let document_index =
        HashMap::from([(target.document_id, target.clone()), (unrelated.document_id, unrelated)]);
    let mut query_ir = setup_query_ir("Workflow library");
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
    query_ir.target_entities =
        vec![EntityMention { label: "DeltaVariant".to_string(), role: EntityRole::Object }];
    query_ir.conversation_refs = vec![UnresolvedRef {
        surface: "that".to_string(),
        kind: crate::domains::query_ir::ConversationRefKind::Pronoun,
    }];

    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    assert_eq!(
        setup_focus_candidate_document_ids(&query_ir, &document_index, 8),
        [target.document_id]
    );
}

#[test]
fn setup_focus_candidates_compose_short_subject_with_variant_modifier() {
    let target = document_row("target.html", "Subject BetaVariant setup");
    let changelog = document_row("history.md", "Release history");
    let document_index =
        HashMap::from([(target.document_id, target.clone()), (changelog.document_id, changelog)]);
    let mut query_ir = setup_query_ir("Workflow library");
    query_ir.target_types = vec![QueryTargetKind::Concept];
    query_ir.document_focus = None;
    query_ir.target_entities = vec![
        EntityMention { label: "Subject".to_string(), role: EntityRole::Subject },
        EntityMention { label: "BetaVariant".to_string(), role: EntityRole::Modifier },
    ];

    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    let terms = setup_focus_query_identity_terms(&query_ir);
    assert_eq!(terms, ["Subject", "Subject BetaVariant"]);
    assert_eq!(
        setup_focus_candidate_document_ids(&query_ir, &document_index, 8),
        [target.document_id]
    );
}

#[test]
fn setup_focus_candidates_prioritize_compound_entity_identity_for_specific_module() {
    let target = document_row("target.html", "Subject DeltaVariant setup guide");
    let neighboring = document_row("neighbor.html", "Subject BetaVariant setup guide");
    let changelog = document_row("history.md", "Subject DeltaVariant release history");
    let document_index = HashMap::from([
        (target.document_id, target.clone()),
        (neighboring.document_id, neighboring.clone()),
        (changelog.document_id, changelog),
    ]);
    let mut query_ir = setup_query_ir("Workflow library");
    query_ir.target_types =
        vec![QueryTargetKind::Concept, QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    query_ir.document_focus = None;
    query_ir.target_entities = vec![
        EntityMention { label: "subject".to_string(), role: EntityRole::Subject },
        EntityMention { label: "deltavariant".to_string(), role: EntityRole::Subject },
    ];

    assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    assert_eq!(
        setup_focus_query_identity_terms(&query_ir),
        ["subject deltavariant".to_string(), "subject".to_string(), "deltavariant".to_string()]
    );
    let candidates = setup_focus_candidate_document_ids(&query_ir, &document_index, 8);
    assert_eq!(candidates.first(), Some(&target.document_id));
    assert!(!candidates.contains(&neighboring.document_id));
}

#[test]
fn setup_variant_candidates_keep_multiple_primary_subject_documents() {
    let alpha = document_row("alpha.html", "Subject Alpha setup");
    let beta = document_row("beta.html", "Subject Beta setup");
    let screenshot = document_row("subject.png", "Subject Beta screenshot");
    let unrelated = document_row("other.html", "Other setup");
    let unrelated_document_id = unrelated.document_id;
    let document_index = HashMap::from([
        (alpha.document_id, alpha.clone()),
        (beta.document_id, beta.clone()),
        (screenshot.document_id, screenshot),
        (unrelated.document_id, unrelated),
    ]);

    let query_ir = setup_query_ir("Subject");
    assert!(question_requests_setup_variant_evidence("how to configure subject?", Some(&query_ir)));
    let candidates = setup_variant_candidate_document_ids(
        "how to configure subject?",
        Some(&query_ir),
        &document_index,
        8,
    );

    assert!(candidates.contains(&alpha.document_id));
    assert!(candidates.contains(&beta.document_id));
    assert!(!candidates.contains(&unrelated_document_id));
}

#[test]
fn setup_variant_evidence_skips_unambiguous_versioned_procedure() {
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];

    assert!(!question_requests_setup_variant_evidence(
        "how to update Sample Target?",
        Some(&query_ir)
    ));
    assert!(!query_ir_requests_setup_variant_anchor_reservation(&query_ir));
}

#[test]
fn setup_variant_evidence_allows_named_focus_literals_but_skips_exact_literals() {
    let mut query_ir = setup_query_ir("Subject Alpha");
    query_ir.document_focus = None;
    query_ir.target_types = vec![
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::Procedure,
    ];
    query_ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    query_ir.literal_constraints =
        vec![LiteralSpan { text: "Subject Alpha".to_string(), kind: LiteralKind::Other }];

    assert!(question_requests_setup_variant_evidence(
        "how to configure subject alpha?",
        Some(&query_ir)
    ));
    assert!(question_requests_setup_variant_evidence(
        "how to configure subject alpha variants with package, command, settings files, sections, and main parameters",
        Some(&query_ir)
    ));

    query_ir.literal_constraints =
        vec![LiteralSpan { text: "/etc/subject-alpha.conf".to_string(), kind: LiteralKind::Path }];
    assert!(!question_requests_setup_variant_evidence(
        "how to configure subject alpha?",
        Some(&query_ir)
    ));
}

#[test]
fn setup_variant_evidence_skips_specific_compound_module_focus() {
    let mut query_ir = setup_query_ir("Workflow library");
    query_ir.target_types =
        vec![QueryTargetKind::Concept, QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    query_ir.document_focus = None;
    query_ir.target_entities = vec![
        EntityMention { label: "Subject".to_string(), role: EntityRole::Subject },
        EntityMention { label: "DeltaVariant".to_string(), role: EntityRole::Subject },
    ];

    assert!(!question_requests_setup_variant_evidence(
        "how to configure Subject DeltaVariant?",
        Some(&query_ir)
    ));
    assert!(!question_requests_versioned_update_procedure_evidence(
        "how to configure Subject DeltaVariant?",
        Some(&query_ir)
    ));
}

#[test]
fn setup_variant_document_family_deduplicates_same_environment_variant() {
    let query_ir = setup_query_ir("Subject");
    let query_terms = setup_variant_query_terms("how to configure subject?", Some(&query_ir));
    let alpha = document_row("alpha.html", "Subject Alpha setup guide");
    let alpha_new = document_row("alpha-new.html", "Subject Alpha updated protocol guide");
    let beta = document_row("beta.html", "Subject Beta setup guide");
    let document_index = HashMap::from([
        (alpha.document_id, alpha.clone()),
        (alpha_new.document_id, alpha_new.clone()),
        (beta.document_id, beta.clone()),
    ]);
    let candidate_document_ids = vec![alpha.document_id, alpha_new.document_id, beta.document_id];
    let family_model =
        setup_variant_family_model(&candidate_document_ids, &document_index, &query_terms);

    assert_eq!(
        setup_variant_document_family(&alpha, &query_terms, &family_model),
        setup_variant_document_family(&alpha_new, &query_terms, &family_model)
    );
    assert_ne!(
        setup_variant_document_family(&alpha, &query_terms, &family_model),
        setup_variant_document_family(&beta, &query_terms, &family_model)
    );
}

#[test]
fn low_confidence_untyped_ir_does_not_enable_setup_focus_lane() {
    let mut query_ir = setup_query_ir("Sample Subject");
    query_ir.act = QueryAct::Describe;
    query_ir.confidence = 0.25;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.document_focus = None;

    assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
}
