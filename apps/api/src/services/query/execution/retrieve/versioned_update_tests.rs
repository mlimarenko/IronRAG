use super::test_prelude::*;

#[test]
fn troubleshooting_request_skips_generic_update_runbook_lanes() {
    let mut query_ir = setup_query_ir("Sample operation was already completed");
    query_ir.target_types = vec![
        QueryTargetKind::Procedure,
        QueryTargetKind::Troubleshooting,
        QueryTargetKind::Remediation,
        QueryTargetKind::ErrorMessage,
    ];
    query_ir.literal_constraints = vec![LiteralSpan {
        text: "sample operation was already completed".to_string(),
        kind: LiteralKind::Other,
    }];

    assert!(!question_requests_versioned_update_procedure_evidence(
        "What should I do when the error says sample operation was already completed?",
        Some(&query_ir),
    ));
    assert!(!query_ir_requests_versioned_update_procedure_context("", &query_ir));
}

#[test]
fn typed_procedure_without_typed_subject_does_not_infer_command_seed() {
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities.clear();
    query_ir.document_focus = None;
    query_ir.retrieval_query = Some("opaque raw prose".to_string());

    assert!(versioned_update_procedure_focus_model(&query_ir).is_none());
    assert!(!query_ir_requests_versioned_update_procedure_context("opaque raw prose", &query_ir,));
}

#[test]
fn setup_variant_candidates_prioritize_subject_identity_over_generic_setup_titles() {
    let alpha = document_row("alpha-subject.html", "Subject Alpha setup guide");
    let beta = document_row("beta-subject.html", "Subject Beta integration settings");
    let generic = document_row("display-settings.html", "Settings appearance setup guide");
    let document_index = HashMap::from([
        (generic.document_id, generic.clone()),
        (alpha.document_id, alpha.clone()),
        (beta.document_id, beta.clone()),
    ]);
    let mut query_ir = setup_query_ir("Subject");
    query_ir.document_focus = None;
    query_ir.target_entities =
        vec![EntityMention { label: "Subject".to_string(), role: EntityRole::Subject }];
    query_ir.retrieval_query = Some("how to configure Subject variants".to_string());

    let candidates = setup_variant_candidate_document_ids(
        "how to configure Subject?",
        Some(&query_ir),
        &document_index,
        3,
    );

    assert!(candidates.contains(&alpha.document_id));
    assert!(candidates.contains(&beta.document_id));
    assert!(!candidates.contains(&generic.document_id));
}

#[test]
fn setup_variant_candidates_weight_rare_identity_terms_over_common_setup_terms() {
    let target = document_row("target.html", "Subject Alpha connector guide");
    let generic_a = document_row("generic-a.html", "Configure display settings");
    let generic_b = document_row("generic-b.html", "Configure server appearance");
    let generic_c = document_row("generic-c.html", "Configure endpoint layout");
    let document_index = HashMap::from([
        (generic_a.document_id, generic_a.clone()),
        (generic_b.document_id, generic_b.clone()),
        (generic_c.document_id, generic_c.clone()),
        (target.document_id, target.clone()),
    ]);
    let mut query_ir = setup_query_ir("Workflow library");
    query_ir.document_focus = None;
    query_ir.target_entities = Vec::new();
    query_ir.retrieval_query = Some("how to configure subject?".to_string());

    let candidates = setup_variant_candidate_document_ids(
        "how to configure subject?",
        Some(&query_ir),
        &document_index,
        1,
    );

    assert_eq!(candidates, [target.document_id]);
}

#[test]
fn concept_only_procedure_query_fails_closed_without_version_signal() {
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    query_ir.retrieval_query = Some("how to update Sample Target".to_string());

    assert!(!question_requests_versioned_update_procedure_evidence(
        "how to update Sample Target?",
        Some(&query_ir)
    ));
}

#[test]
fn raw_question_does_not_activate_procedure_runbook_without_typed_identity() {
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities.clear();
    query_ir.document_focus = None;
    query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

    assert!(versioned_update_procedure_focus_model(&query_ir).is_none());
    assert!(!question_requests_versioned_update_procedure_evidence(
        "how to update Sample Target?",
        Some(&query_ir),
    ));
}

#[test]
fn identifier_literal_is_an_explicit_procedure_target_identity() {
    let mut query_ir = setup_query_ir("unfocused");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Release];
    query_ir.target_entities.clear();
    query_ir.document_focus = None;
    query_ir.literal_constraints =
        vec![LiteralSpan { text: "Sample Process".to_string(), kind: LiteralKind::Identifier }];

    let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();

    assert!(
        focus_model
            .target_identity_sequences
            .iter()
            .any(|sequence| sequence == &vec!["sample".to_string(), "process".to_string()])
    );
    assert!(query_ir_requests_versioned_update_procedure_context("", &query_ir));

    query_ir.literal_constraints[0].kind = LiteralKind::Other;
    assert!(versioned_update_procedure_focus_model(&query_ir).is_some());

    query_ir.literal_constraints[0].kind = LiteralKind::Version;
    assert!(versioned_update_procedure_focus_model(&query_ir).is_none());
}

#[test]
fn typed_versioned_procedure_accepts_exact_identity_and_formal_steps() {
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();
    let evidence = versioned_update_procedure_text_evidence(
        "Sample Target reference",
        concat!(
            "1. alpha-admin prepare --target=/srv/alpha\n",
            "2. alpha-admin verify --format=json",
        ),
        &focus_model,
    );

    assert!(query_ir_requests_versioned_update_procedure_context("", &query_ir));
    assert!(evidence.label_has_target_identity);
    assert_eq!(evidence.ordered_step_score, 2);
    assert!(versioned_update_procedure_evidence_supports_runbook(evidence));
}

#[test]
fn typed_versioned_procedure_rejects_fuzzy_identity_and_unstructured_prose() {
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Release];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();
    let fuzzy_identity = versioned_update_procedure_text_evidence(
        "Sample Targets reference",
        "1. alpha-admin prepare\n2. alpha-admin verify",
        &focus_model,
    );
    let prose_only = versioned_update_procedure_text_evidence(
        "Sample Target reference",
        "A paragraph describes a possible lifecycle transition without formal steps.",
        &focus_model,
    );

    assert!(!fuzzy_identity.label_has_target_identity);
    assert!(!versioned_update_procedure_text_has_target_identity_sequence(
        "1. alpha-admin prepare\n2. alpha-admin verify",
        &focus_model,
    ));
    assert!(!versioned_update_procedure_evidence_supports_runbook(fuzzy_identity,));
    assert!(!versioned_update_procedure_evidence_supports_runbook(prose_only,));
}

#[test]
fn typed_versioned_procedure_rejects_unbound_materialization_script() {
    let mut query_ir = setup_query_ir("Sample Target");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
    let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();
    let evidence = versioned_update_procedure_text_evidence(
        "Sample Target reference",
        concat!(
            "curl https://example.invalid/runner.sh -o /tmp/runner.sh\n",
            "chmod +x /tmp/runner.sh\n",
            "/tmp/runner.sh",
        ),
        &focus_model,
    );

    assert!(evidence.has_setup_script_signature);
    assert_eq!(evidence.ordered_step_score, 0);
    assert!(!versioned_update_procedure_evidence_supports_runbook(evidence,));
}

#[test]
fn command_dense_excerpt_accepts_structural_product_cli_lines() {
    let excerpt = command_dense_excerpt_for(
        concat!(
            "Sample Subject maintenance\n",
            "1. alpha-admin migrate --target=/opt/alpha\n",
            "2. alpha-admin verify --format=json"
        ),
        400,
    );

    assert!(excerpt.is_some());
}
