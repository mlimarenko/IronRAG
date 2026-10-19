use super::*;

#[test]
fn literal_free_answer_is_not_acceptable_when_context_has_requested_technical_literals() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![QueryTargetKind::Path, QueryTargetKind::ConfigKey];
    let context = "Use `/srv/scans`, then set scan_path = /srv/scans in the share block.";

    assert!(super::answer_omits_expected_technical_literals(
        "The provided context does not include setup details.",
        &ir,
        context,
    ));
    assert!(super::answer_omits_expected_technical_literals(
        "The provided context does not include setup details.",
        &ir,
        "Use `/srv/scans` for the scan directory.",
    ));
    assert!(!super::answer_omits_expected_technical_literals(
        "Use `/srv/scans` for the scan directory.",
        &ir,
        context,
    ));
}

#[test]
fn low_confidence_unfocused_answer_does_not_infer_structural_coverage() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types.clear();
    ir.target_entities.clear();
    ir.literal_constraints.clear();
    ir.temporal_constraints.clear();
    ir.document_focus = None;
    ir.source_slice = None;
    ir.conversation_refs.clear();
    let context = r#"
Retrieved graph evidence
[graph-evidence target="FocusTokenA P0"]
1. Choose "A0", then press "B1".
[graph-evidence target="FocusTokenA P1"]
2. If state is "C2", use "D3".
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="FocusTokenA D0" chunk_index=0] Dialog "E4" offers "F5".
"#;

    assert!(!super::answer_omits_structural_context_coverage(
        "Use the available fallback from the selected source.",
        &ir,
        context,
    ));
    assert!(!super::answer_omits_structural_context_coverage(
        "Choose `A0`, then use `D3` for the alternate state.",
        &ir,
        context,
    ));
}

#[test]
fn structural_anchor_coverage_uses_only_typed_query_focus() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types = vec![QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "FocusTokenA".to_string(), role: EntityRole::Subject }];
    ir.literal_constraints.clear();
    ir.temporal_constraints.clear();
    ir.document_focus = None;
    ir.source_slice = None;
    ir.conversation_refs.clear();
    let context = r#"
Retrieved graph evidence
[graph-evidence target="NoiseTokenZ"]
1. Choose "A0", then press "B1".
[graph-evidence target="FocusTokenA"]
2. If state is "C2", use "D3".
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="FocusTokenA runbook" chunk_index=0] Dialog "E4" offers "F5".
"#;

    assert!(super::answer_omits_structural_context_coverage(
        "Choose `A0`, then press `B1`.",
        &ir,
        context,
    ));
    assert!(!super::answer_omits_structural_context_coverage(
        "Use `C2` with `D3`, then handle `E4` and `F5`.",
        &ir,
        context,
    ));
    let focus = super::structural_coverage_focus_tokens(&ir);
    assert!(focus.contains("focustokena"));
    assert!(!focus.contains("noisetokenz"));
}

#[test]
fn structural_coverage_rejects_a_paraphrased_enum_variant_target() {
    let mut ir = sample_ir_with_act(QueryAct::Enumerate, 0.9, None);
    ir.target_types = vec![QueryTargetKind::State];
    ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Identifier, text: "Queued".to_string() }];
    let context = "FlowStage::Queued => &[FlowStage::Running, FlowStage::Aborted],";

    assert!(super::super::answer::answer_omits_structural_identifier_mapping_target(
        "Which transitions are valid from Queued?",
        "- `Running`\n- `Abort`",
        &ir,
        context,
    ));
    assert!(!super::super::answer::answer_omits_structural_identifier_mapping_target(
        "Which transitions are valid from Queued?",
        "- `FlowStage::Running`\n- `FlowStage::Aborted`",
        &ir,
        context,
    ));

    let mut verification = RuntimeAnswerVerification {
        state: QueryVerificationState::Verified,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };
    super::apply_structural_coverage_warning(
        "Which transitions are valid from Queued?",
        "- `Running`\n- `Abort`",
        &ir,
        context,
        &serde_json::json!({}),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().any(|warning| warning.code == "partial_coverage"));
}

#[test]
fn structural_anchor_coverage_ignores_evidence_chunk_metadata_literals() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types = vec![QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "FocusTokenA".to_string(), role: EntityRole::Subject }];
    ir.literal_constraints.clear();
    ir.temporal_constraints.clear();
    ir.document_focus = None;
    ir.source_slice = None;
    ir.conversation_refs.clear();
    let context = r#"
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="FocusTokenA D0" chunk_index=0] This paragraph has no quoted value.
- [EVIDENCE_CHUNK document="FocusTokenA D1" chunk_index=1] This paragraph also has no config value.
- [EVIDENCE_CHUNK document="FocusTokenA D2" chunk_index=2] This paragraph is descriptive only.
- [EVIDENCE_CHUNK document="FocusTokenA D3" chunk_index=3] This paragraph is still descriptive.
"#;

    assert!(!super::answer_omits_structural_context_coverage(
        "Use the available fallback from the selected source.",
        &ir,
        context,
    ));
}

#[test]
fn structural_anchor_coverage_keeps_unknown_untyped_anchor() {
    assert_eq!(super::normalize_structural_coverage_anchor("answer"), Some("answer".to_string()));
}

#[test]
fn structural_anchor_coverage_excludes_typed_graph_provenance() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types.clear();
    ir.target_entities.clear();
    ir.literal_constraints.clear();
    ir.temporal_constraints.clear();
    ir.document_focus = None;
    ir.source_slice = None;
    ir.conversation_refs.clear();
    let anchors = super::collect_structural_coverage_anchors(
        &ir,
        r#"[graph-evidence target="FocusTokenA" source="opaque://record/7"]
Dialog "A0" offers "B1"."#,
    );

    assert_eq!(anchors.items, vec!["a0", "b1"]);
}

#[test]
fn structural_anchor_coverage_excludes_uri_scheme_syntax() {
    assert_eq!(super::normalize_structural_coverage_anchor("opaque+record:item-7"), None);
}

#[test]
fn structural_anchor_coverage_uses_token_boundaries_for_single_token_anchors() {
    let answer_tokens = crate::services::query::text_match::normalized_alnum_tokens("A10x B20x", 1);

    assert!(!super::structural_answer_contains_anchor("a10x b20x", &answer_tokens, "A10",));
    assert!(super::structural_answer_contains_anchor("a10x b20x", &answer_tokens, "A10x",));
}

#[test]
fn broad_procedure_uses_variant_coverage_instead_of_generic_structural_guard() {
    let ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.9, None);
    let mut verification = RuntimeAnswerVerification {
        state: QueryVerificationState::PartiallySupported,
        warnings: vec![crate::domains::query::QueryVerificationWarning {
            code: "semantic_verification_partial".to_string(),
            message: "Synthetic semantic verifier warning.".to_string(),
            related_segment_id: None,
            related_fact_id: None,
        }],
        unsupported_literals: Vec::new(),
    };

    super::apply_structural_coverage_warning(
        "",
        "Variant Alpha uses its documented procedure. Variant Beta uses its separate procedure.",
        &ir,
        r#"
[graph-evidence target="workflow module Alpha"] Choose "A0", then press "B1".
[graph-evidence target="workflow module Beta"] If state is "C2", use "D3".
"#,
        &serde_json::json!({}),
        &mut verification,
    );

    assert!(ir.requests_broad_procedure_variant_coverage());
    assert!(!verification.warnings.iter().any(|warning| warning.code == "partial_coverage"));
}

#[test]
fn focused_procedure_keeps_generic_structural_coverage_guard() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.9, None);
    ir.document_focus = Some(DocumentHint { hint: "workflow module Alpha".to_string() });
    let mut verification = RuntimeAnswerVerification {
        state: QueryVerificationState::Verified,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };

    super::apply_structural_coverage_warning(
        "",
        "Use the available fallback from the selected source.",
        &ir,
        r#"
[graph-evidence target="workflow module Alpha P0"] Choose "A0", then press "B1".
[graph-evidence target="workflow module Alpha P1"] If state is "C2", use "D3".
"#,
        &serde_json::json!({}),
        &mut verification,
    );

    assert!(!ir.requests_broad_procedure_variant_coverage());
    assert!(verification.warnings.iter().any(|warning| warning.code == "partial_coverage"));
}

#[test]
fn structural_anchor_coverage_warning_marks_verified_answer_partial() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types = vec![QueryTargetKind::Procedure];
    ir.target_entities =
        vec![EntityMention { label: "FocusTokenA".to_string(), role: EntityRole::Subject }];
    ir.literal_constraints.clear();
    ir.temporal_constraints.clear();
    ir.document_focus = None;
    ir.source_slice = None;
    ir.conversation_refs.clear();
    let mut verification = RuntimeAnswerVerification {
        state: QueryVerificationState::Verified,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };

    super::apply_structural_coverage_warning(
        "",
        "Use the available fallback from the selected source.",
        &ir,
        r#"
[graph-evidence target="FocusTokenA P0"] Choose "A0", then press "B1".
[graph-evidence target="FocusTokenA P1"] If state is "C2", use "D3".
"#,
        &serde_json::json!({}),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(verification.warnings.iter().any(|warning| warning.code == "partial_coverage"));
}

#[test]
fn structural_anchor_coverage_does_not_expand_issue_local_remediation_scope() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.9, None);
    ir.target_types = vec![
        QueryTargetKind::Procedure,
        QueryTargetKind::Troubleshooting,
        QueryTargetKind::Remediation,
        QueryTargetKind::ErrorMessage,
    ];
    ir.literal_constraints = vec![LiteralSpan {
        text: "sample operation was already completed".to_string(),
        kind: LiteralKind::Other,
    }];
    let mut verification = RuntimeAnswerVerification {
        state: QueryVerificationState::Verified,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };

    super::apply_structural_coverage_warning(
        "",
        "For this exact error, close the message, replace the item, and contact support.",
        &ir,
        r#"
[EVIDENCE_CHUNK] Sample operation was already completed; unrelated branch uses "A0" and "B1".
[EVIDENCE_CHUNK] Sample operation was already completed; another branch uses "C2" and "D3".
"#,
        &serde_json::json!({}),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn structural_anchor_coverage_preserves_deterministic_anchor_inventory_answers() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
    ];
    let mut verification = RuntimeAnswerVerification {
        state: QueryVerificationState::Verified,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };
    let ru_labels = crate::services::query::i18n::RU_DETERMINISTIC_ANSWER_LABELS;
    let answer = format!(
        "**{}:**\n\n**{}:** **Subject Alpha setup**\n- **{}:** `/opt/alpha/connector.conf`\n- **{}:** `endpointUrl = https://example.invalid`",
        ru_labels.variants, ru_labels.source, ru_labels.path, ru_labels.parameter
    );

    super::apply_structural_coverage_warning(
        "",
        &answer,
        &ir,
        r#"
[graph-evidence target="Subject Alpha P0"] Choose "A0", then press "B1".
[graph-evidence target="Subject Alpha P1"] If state is "C2", use "D3".
"#,
        &serde_json::json!({
            "deterministic": true,
            "answer_kind": super::AnswerKind::SetupConfigurationAnchor.as_str(),
        }),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn configure_setup_answer_requires_focused_parameter_coverage() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
    ];
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Paths: `/opt/alpha/modules/connector/connector.conf`
  Parameters: `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`
- Document: `Subject Beta setup`
  Parameters: `otherEndpoint`, `otherSecret`, `otherTimeout`, `otherFlag`
"#;

    assert!(super::answer_omits_expected_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`.",
        &ir,
        context,
    ));
    assert!(super::answer_omits_expected_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/display/display.ini`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`.",
        &ir,
        context,
    ));
    let expected = super::collect_focused_context_parameter_literals(context, &ir, 32);
    let actual = super::collect_intended_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`.",
        super::TechnicalLiteralIntent {
            wants_parameters: true,
            ..super::TechnicalLiteralIntent::default()
        },
        32,
    );
    assert!(
        expected.iter().all(|literal| actual.contains(literal)),
        "expected={expected:?} actual={actual:?}"
    );
    assert!(!super::answer_omits_expected_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`.",
        &ir,
        context,
    ));
}

#[test]
fn configure_setup_answer_requires_focused_section_coverage() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
    ];
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Paths: `/opt/alpha/modules/connector/connector.conf`
  Sections:
    - `[CFG]`
    - `[UI.ScanPanel.qrCode]`
  Parameters:
    - `endpointUrl`
    - `partnerId`
"#;

    assert!(super::answer_omits_expected_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`.",
        &ir,
        context,
    ));
    assert!(!super::answer_omits_expected_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, use `[CFG]` and `[UI.ScanPanel.qrCode]`, and set `endpointUrl`, `partnerId`.",
        &ir,
        context,
    ));
}

#[test]
fn configure_setup_parameter_coverage_ignores_aggregate_metadata_values() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
    ];
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Paths:
    - `/opt/alpha/modules/connector/connector.conf`
  Parameters:
    - `endpointUrl`
    - `partnerId`
    - `secretKey`
    - `retryInterval`

Prepared segments
- metadata_block > Subject Alpha setup: Table Summary | Sheet: Module setup | Column: Name | Value Kind: categorical | Most Frequent Values: retryTimeout; sendDetails; auditMode
- paragraph > Subject Alpha setup: The `sendDetails` flag controls whether workflow details are forwarded.
"#;

    let expected = super::collect_focused_context_parameter_literals(context, &ir, 32);

    assert!(!expected.contains("retryTimeout"));
    assert!(!expected.contains("sendDetails"));
    assert!(!expected.contains("auditMode"));

    assert!(super::answer_omits_expected_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`.",
        &ir,
        context,
    ));
    assert!(!super::answer_omits_expected_technical_literals(
        "Install `alpha-connector`, edit `/opt/alpha/modules/connector/connector.conf`, and set `endpointUrl`, `partnerId`, `secretKey`, `retryInterval`.",
        &ir,
        context,
    ));
}

#[test]
fn configure_setup_parameter_coverage_ignores_prose_only_literals() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
    ];
    ir.target_entities =
        vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
    let context = r#"
Exact technical literals
- Document: `Subject Alpha setup`
  Parameters:
    - `endpointUrl`
    - `partnerId`
    - `secretKey`
    - `retryTimeout`

Prepared segments
- paragraph > Subject Alpha setup: Operators may inspect `LOG_LEVEL` while diagnosing a setup issue.
- list_item > Subject Alpha setup: Keep `backupBeforeChange` enabled for maintenance notes.
"#;

    let expected = super::collect_focused_context_parameter_literals(context, &ir, 32);

    assert!(!expected.contains("LOG_LEVEL"));
    assert!(!expected.contains("backupBeforeChange"));
    assert!(!super::answer_omits_expected_technical_literals(
        "Set `endpointUrl`, `partnerId`, `secretKey`, and `retryTimeout`.",
        &ir,
        context,
    ));
}

#[test]
fn configure_setup_answer_requires_assignment_example_coverage() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey];
    let context = r#"
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="Subject Alpha setup" chunk_index=2] Example:
[Main]
alphaFlag = true
[UI.Component]
visible = true
"#;

    assert!(super::answer_omits_expected_technical_literals(
        "Set `alphaFlag = true`.",
        &ir,
        context,
    ));
    assert!(!super::answer_omits_expected_technical_literals(
        "Set `alphaFlag = true` and `visible = true`.",
        &ir,
        context,
    ));
}

#[test]
fn low_confidence_describe_answer_requires_assignment_example_coverage() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.25, None);
    ir.target_types.clear();
    ir.target_entities.clear();
    let context = r#"
EVIDENCE_CHUNK blocks
- [EVIDENCE_CHUNK document="Subject Alpha setup" chunk_index=2] Example:
[Main]
alphaFlag = true
[Check]
printSlip = false
"#;

    assert!(super::answer_omits_expected_technical_literals(
        "The context contains `alphaFlag = true`.",
        &ir,
        context,
    ));
    assert!(!super::answer_omits_expected_technical_literals(
        "The context contains `alphaFlag = true` and `printSlip = false`.",
        &ir,
        context,
    ));
}

#[test]
fn targeted_technical_query_without_focus_context_skips_initial_fast_path() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![QueryTargetKind::Path, QueryTargetKind::ConfigKey];
    ir.target_entities = vec![
        EntityMention { label: "RareProtocol scan share".to_string(), role: EntityRole::Subject },
        EntityMention { label: "RareProtocol daemon".to_string(), role: EntityRole::Object },
    ];
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context =
        "[EVIDENCE_CHUNK document=\"nearby\"] Sample Subject setup uses `/srv/scans`.".to_string();

    assert!(
        !super::should_use_single_shot_answer(
            "How do I configure the RareProtocol scan share?",
            &prepared,
            None,
        ),
        "technical initial single-shot needs focus evidence before it can finalize without preflight"
    );
}

#[test]
fn targeted_technical_single_shot_answer_needs_context_backed_focus() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![QueryTargetKind::Path, QueryTargetKind::ConfigKey];
    ir.target_entities = vec![
        EntityMention { label: "RareProtocol scan share".to_string(), role: EntityRole::Subject },
        EntityMention { label: "RareProtocol daemon".to_string(), role: EntityRole::Object },
    ];
    let unrelated_context =
        "[EVIDENCE_CHUNK document=\"nearby\"] Sample Subject setup uses `/srv/scans`.";
    let focused_context =
        "[EVIDENCE_CHUNK document=\"target\"] RareProtocol daemon setup uses `/srv/scans`.";

    assert!(super::single_shot_lacks_query_focus_support(
        "The context has no RareProtocol details, but mentions `/srv/scans`.",
        &ir,
        unrelated_context,
    ));
    assert!(super::single_shot_lacks_query_focus_support(
        "Use `/srv/scans` for the scan directory.",
        &ir,
        focused_context,
    ));
    assert!(!super::single_shot_lacks_query_focus_support(
        "RareProtocol daemon uses `/srv/scans` for the scan directory.",
        &ir,
        focused_context,
    ));
}

#[test]
fn plain_literal_does_not_require_single_shot_focus_support() {
    let mut ir = sample_ir_with_act(QueryAct::RetrieveValue, 0.8, None);
    ir.target_types = vec![QueryTargetKind::Concept];
    ir.target_entities = Vec::new();
    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "alpha".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Identifier,
    }];

    assert!(!super::query_requires_single_shot_focus_support(&ir));

    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "callbackUrl".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Identifier,
    }];

    assert!(super::query_requires_single_shot_focus_support(&ir));
}

#[test]
fn configure_how_exact_literal_requires_single_shot_focus_support() {
    let mut ir = sample_ir_with_act(QueryAct::ConfigureHow, 0.8, None);
    ir.target_types = vec![QueryTargetKind::Concept];
    ir.target_entities = Vec::new();
    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "alpha".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Other,
    }];

    assert!(!super::query_requires_single_shot_focus_support(&ir));

    ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
        text: "2.4.1".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Version,
    }];

    assert!(super::query_requires_single_shot_focus_support(&ir));
}

#[test]
fn compare_without_structural_operands_skips_initial_fast_path() {
    let prepared = prepared_for_single_shot(sample_ir_with_act(QueryAct::Compare, 0.8, None));

    assert!(
        !super::should_use_single_shot_answer("q", &prepared, None),
        "compare questions without covered operands should wait for evidence preflight evidence"
    );
}

#[test]
fn compare_uses_single_shot_when_prepared_context_covers_operands() {
    let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
    ir.comparison = Some(ComparisonSpec {
        a: Some("Sample Subject".to_string()),
        b: Some("Beta Suite".to_string()),
        dimension: "capability".to_string(),
    });
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context = "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"alpha\"] Sample Subject stores audit events.\n\
[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"beta\"] Beta Suite stores billing events."
            .to_string();

    assert!(
        super::should_use_single_shot_answer("q", &prepared, None),
        "compare can use the prepared single-shot context when every IR operand is covered"
    );
}

#[test]
fn compare_uses_single_shot_when_prepared_context_partially_covers_operands() {
    let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
    ir.comparison = Some(ComparisonSpec {
        a: Some("Sample Subject".to_string()),
        b: Some("Beta Suite".to_string()),
        dimension: "capability".to_string(),
    });
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context =
            "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"alpha\"] Sample Subject stores audit events."
                .to_string();

    assert!(
        super::should_use_single_shot_answer("q", &prepared, None),
        "one-sided compare evidence should produce a fast grounded partial answer instead of a second retrieval pass"
    );
}

#[test]
fn compare_skips_initial_fast_path_when_no_operand_is_covered() {
    let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
    ir.comparison = Some(ComparisonSpec {
        a: Some("Sample Subject".to_string()),
        b: Some("Beta Suite".to_string()),
        dimension: "capability".to_string(),
    });
    let mut prepared = prepared_for_single_shot(ir);
    prepared.answer_context =
            "[EVIDENCE_CHUNK scope=excerpt coverage=sampled document=\"gamma\"] Gamma Console stores audit events."
                .to_string();

    assert!(
        !super::should_use_single_shot_answer("q", &prepared, None),
        "compare with zero covered operands should not use the initial fast path"
    );
}

#[test]
fn comparison_coverage_metadata_does_not_count_as_grounding_evidence() {
    let mut ir = sample_ir_with_act(QueryAct::Compare, 0.8, None);
    ir.comparison = Some(ComparisonSpec {
        a: Some("Sample Subject".to_string()),
        b: Some("Beta Suite".to_string()),
        dimension: "capability".to_string(),
    });
    let coverage = super::compare_operands_covered_by_context(
        &ir,
        "COMPARISON_COVERAGE status=partial\n\
- covered_operand: Sample Subject\n\
- uncovered_operand: Beta Suite",
    );

    assert!(
        matches!(coverage, super::EvidenceCoverage::Insufficient("compare_evidence_empty")),
        "internal coverage markers must not become synthetic evidence"
    );
}
