use super::test_support::*;
use super::*;

#[test]
fn setup_configuration_anchor_answer_lists_generic_variants() {
    let mut alpha = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\nsample-runner --install sample-unit\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha/alpha.ini in section [AlphaQrp].\n\
             url = http://localhost\nprimaryKey = \"\"",
    );
    alpha.document_label = "Subject Alpha setup".to_string();
    let mut beta = evidence_chunk(
        2,
        Some("paragraph"),
        "Module configuration\nsample-install beta-subject\n\
             sample-configure beta-subject\n\
             Settings are defined in /opt/subject/beta/beta.conf in section [BetaQrp].\n\
             timeout = 60",
    );
    beta.document_label = "Subject Beta setup".to_string();
    let mut unrelated = evidence_chunk(
        3,
        Some("paragraph"),
        "Integrator manual: create one workflow method and set a token in the UI.",
    );
    unrelated.document_label = "Generic integrator manual".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Subject?",
        &configure_how_focus_ir("Subject"),
        &[unrelated, alpha, beta],
    )
    .expect("setup configuration answer");

    assert!(answer.contains("**Setup variants:**"));
    assert!(answer.contains("Subject Alpha setup"));
    assert!(answer.contains("alpha-subject"));
    assert!(answer.contains("/opt/subject/alpha/alpha.ini"));
    assert!(answer.contains("AlphaQrp"));
    assert!(answer.contains("Subject Beta setup"));
    assert!(answer.contains("beta-subject"));
    assert!(answer.contains("/opt/subject/beta/beta.conf"));
    assert!(answer.contains("BetaQrp"));
    assert!(!answer.contains("token in the UI"));
}

#[test]
fn setup_configuration_anchor_answer_accepts_procedure_artifact_ir_with_config_evidence() {
    let mut query_ir = configure_how_focus_ir("Sample Connector");
    query_ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];

    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Connector configuration\n\
             sample-configure --target=sample-connector\n\
             Settings are stored in /etc/sample/connector.conf in section [Main].\n\
             sampleMerchantId = identifier issued by the provider.",
    );
    chunk.document_label = "Sample Connector setup guide".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Sample Connector?",
        &query_ir,
        &[chunk],
    )
    .expect("setup configuration answer");

    assert!(answer.contains("Sample Connector setup guide"));
    assert!(answer.contains("sample-configure"));
    assert!(answer.contains("/etc/sample/connector.conf"));
    assert!(answer.contains("Main"));
    assert!(answer.contains("sampleMerchantId"));
}

#[test]
fn setup_configuration_anchor_answer_accepts_subject_entity_config_evidence() {
    let mut query_ir = configure_how_focus_ir("Sample Connector");
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "Sample Connector".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];

    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Connector settings are stored in /etc/sample/connector.conf in section [Main].",
    );
    anchor.document_id = document_id;
    anchor.revision_id = revision_id;
    anchor.document_label = "Sample Connector setup guide".to_string();
    let mut parameters =
        evidence_chunk(2, Some("code_block"), "[Main]\n;merchantId = \"\"\n;printReceipt = false");
    parameters.document_id = document_id;
    parameters.revision_id = revision_id;
    parameters.document_label = anchor.document_label.clone();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Sample Connector?",
        &query_ir,
        &[anchor, parameters],
    )
    .expect("subject-entity setup configuration answer");

    assert!(answer.contains("Sample Connector setup guide"), "{answer}");
    assert!(answer.contains("/etc/sample/connector.conf"), "{answer}");
    assert!(answer.contains("Main"), "{answer}");
    assert!(answer.contains("merchantId"), "{answer}");
    assert!(answer.contains("printReceipt"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_extracts_embedded_command_anchor() {
    let mut query_ir = configure_how_focus_ir("Sample Connector");
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Parameter];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "Sample Connector".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Connector configuration\n\
             Install the package with sample-install --package=sample-connector.\n\
             Configuration command sample-reconfigure --package=sample-connector.\n\
             Settings are defined in /etc/sample/connector.conf in section [Main].\n\
             sampleMerchantId = \"\"\n\
             sampleTerminalKey = \"\"",
    );
    chunk.document_label = "Sample Connector setup guide".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Sample Connector?",
        &query_ir,
        &[chunk],
    )
    .expect("setup configuration answer");

    assert!(answer.contains("sample-reconfigure --package=sample-connector"), "{answer}");
    assert!(answer.contains("/etc/sample/connector.conf"), "{answer}");
    assert!(answer.contains("[Main]"), "{answer}");
    assert!(answer.contains("sampleMerchantId"), "{answer}");
}

#[test]
fn setup_configuration_command_literals_reject_mixed_script_prose_fragments() {
    let commands = extract_setup_configuration_command_literals(
        "QR-code text appears in the user interface.\n\
              QR-код для оплаты отображается на экране.\n\
              Configuration command sample-reconfigure --package=sample-connector.\n\
             Section [Main] contains sampleMerchantId.",
        8,
    );

    assert!(
        commands.iter().any(|command| command == "sample-reconfigure --package=sample-connector")
    );
    assert!(!commands.iter().any(|command| command.contains("QR-код")), "{commands:?}");
    assert!(!commands.iter().any(|command| command.starts_with("Section ")), "{commands:?}");
}

#[test]
fn setup_configuration_command_literals_require_formal_command_signals() {
    let ambiguous = extract_setup_configuration_command_literals(
        "address should be configured as https://localhost/api\n\
             package-name . sample-install package-name\n\
             sample-configure package-name",
        8,
    );
    assert!(ambiguous.is_empty(), "{ambiguous:?}");

    let commands = extract_setup_configuration_command_literals(
        "sample-install --package=package-name\n\
             sample-configure package=package-name",
        8,
    );

    assert_eq!(
        commands,
        vec!["sample-install --package=package-name", "sample-configure package=package-name",]
    );
}

#[test]
fn setup_configuration_command_literals_reject_artifact_headed_text() {
    let commands = extract_setup_configuration_command_literals(
        "/work/sample/config.ini contains mode=strict\n\
             https://example.invalid/schema describes mode=strict\n\
             runner-tool --mode=strict",
        8,
    );

    assert_eq!(commands, vec!["runner-tool --mode=strict"]);
}

#[test]
fn setup_configuration_anchor_answer_renders_beyond_four_variants() {
    let mut query_ir = configure_how_focus_ir("DeltaVariant");
    query_ir.target_types = vec![
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::Package,
        QueryTargetKind::Procedure,
    ];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "DeltaVariant".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    let chunks = (1..=5)
            .map(|index| {
                let mut chunk = evidence_chunk(
                    index,
                    Some("paragraph"),
                    &format!(
                        "DeltaVariant module {index} configuration\n\
                         sample-configure --target=delta-module-{index}\n\
                         Settings are defined in /opt/delta/module-{index}.conf in section [Delta{index}]."
                    ),
                );
                chunk.document_label = format!("DeltaVariant setup variant {index}");
                chunk
            })
            .collect::<Vec<_>>();

    let answer =
        build_setup_configuration_anchor_answer("configure DeltaVariant", &query_ir, &chunks)
            .expect("setup configuration answer");

    assert!(answer.contains("DeltaVariant setup variant 5"), "{answer}");
    assert!(answer.contains("sample-configure --target=delta-module-5"), "{answer}");
    assert!(answer.contains("/opt/delta/module-5.conf"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_suppresses_variant_heading_for_duplicate_anchors() {
    let mut primary = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\nsample-install alpha-subject\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha/alpha.ini in section [AlphaMain].\n\
             url = http://localhost",
    );
    primary.document_label = "Subject Alpha setup".to_string();
    let mut secondary = evidence_chunk(
        2,
        Some("paragraph"),
        "Operational note\nsample-install alpha-subject\n\
             Settings file: /opt/subject/alpha/alpha.ini\n\
             timeout = 60",
    );
    secondary.document_label = "Subject Alpha operator guide".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Subject Alpha?",
        &configure_how_focus_ir("Subject Alpha"),
        &[primary, secondary],
    )
    .expect("setup configuration answer");

    assert!(!answer.contains("**Setup variants:**"), "{answer}");
    assert!(answer.contains("Subject Alpha setup"));
    assert!(answer.contains("Subject Alpha operator guide"));
    assert!(answer.contains("alpha-subject"));
    assert!(answer.contains("/opt/subject/alpha/alpha.ini"));
}

#[test]
fn setup_configuration_anchor_answer_skips_broad_retrieve_value_without_setup_target() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "Alpha reference".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Module reference\n\
             | Name | Type | Description |\n\
             | alpha_limit | integer | Maximum retained records |\n\
             | beta_window | duration | Rolling measurement window |",
    );
    chunk.document_label = "Neutral reference".to_string();

    assert!(
        build_setup_configuration_anchor_answer(
            "What value is listed for the alpha limit?",
            &query_ir,
            &[chunk],
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_answer_skips_focused_value_status_lookup() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.scope = QueryScope::SingleDocument;
    query_ir.target_types =
        vec![QueryTargetKind::Document, QueryTargetKind::ConfigKey, QueryTargetKind::ErrorCode];
    query_ir.document_focus =
        Some(crate::domains::query_ir::DocumentHint { hint: "reference.json".to_string() });
    query_ir.target_entities.push(crate::domains::query_ir::EntityMention {
        label: "adjustment record".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    });
    query_ir.literal_constraints.push(crate::domains::query_ir::LiteralSpan {
        text: "reason_code".to_string(),
        kind: crate::domains::query_ir::LiteralKind::Identifier,
    });
    let mut chunk = evidence_chunk(
        1,
        Some("source_unit"),
        "fields: records.adjustment.reason_code.enum=receiving,return,damage,loss,correction; \
             records.adjustment.responses.422.description=Insufficient balance; \
             records.adjustment.section=[A-Z0-9-]; records.adjustment.parameter=reason_code",
    );
    chunk.document_label = "reference.json".to_string();

    assert!(
        build_setup_configuration_anchor_answer(
            "What reason codes are valid, and what status indicates insufficient balance?",
            &query_ir,
            &[chunk],
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_answer_filters_structured_distractors_by_focus_token() {
    let mut irrelevant = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\nsample-install gamma-adapter\n\
             sample-configure gamma-adapter\n\
             Settings are defined in /opt/gamma/gamma.conf in section [GammaAdapter].\n\
             endpoint = http://localhost",
    );
    irrelevant.document_label = "Gamma Adapter setup reference".to_string();
    let mut relevant = evidence_chunk(
        2,
        Some("paragraph"),
        "Module configuration\nsample-install alpha-qps\n\
             sample-configure alpha-qps\n\
             Settings are defined in /opt/qps/alpha.conf in section [QpsAlpha].\n\
             primaryKey = \"\"",
    );
    relevant.document_label = "QPS Alpha setup reference".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure QPS?",
        &configure_how_focus_ir("QPS"),
        &[irrelevant, relevant],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("QPS Alpha setup reference"));
    assert!(answer.contains("alpha-qps"));
    assert!(answer.contains("/opt/qps/alpha.conf"));
    assert!(!answer.contains("Gamma Adapter setup reference"));
    assert!(!answer.contains("gamma-adapter"));
}

#[test]
fn setup_configuration_anchor_answer_prefers_subject_label_over_body_only_match() {
    let mut body_only = evidence_chunk(
        1,
        Some("paragraph"),
        "Subject compatibility note\nsample-install dialog-tools\n\
             sample-configure dialog-tools\n\
             Settings are defined in /opt/dialog/dialog.conf in section [DialogQuestion].\n\
             subjectTimeout = 30\nquestionText = \"\"",
    );
    body_only.document_label = "Question dialog configuration guide".to_string();
    let mut focused = evidence_chunk(
        2,
        Some("paragraph"),
        "Workflow module configuration\nsample-runner --install sample-unit\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha.conf in section [AlphaQrp].\n\
             primaryKey = \"\"",
    );
    focused.document_label = "Subject Alpha setup reference".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Subject?",
        &configure_how_focus_ir("Subject"),
        &[body_only, focused],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("Subject Alpha setup reference"), "{answer}");
    assert!(answer.contains("alpha-subject"), "{answer}");
    assert!(!answer.contains("Question dialog configuration guide"), "{answer}");
    assert!(!answer.contains("dialog-tools"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_keeps_short_subject_followup_on_label_identity() {
    let mut stale = evidence_chunk(
        1,
        Some("paragraph"),
        "DeltaVariant compatibility parameter\nsample-runner --install sample-unit\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha.conf in section [AlphaSubject].\n\
             deltaMode = true",
    );
    stale.document_label = "AlphaSubject setup reference".to_string();
    let mut focused = evidence_chunk(
        2,
        Some("paragraph"),
        "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].\n\
             primaryKey = \"\"",
    );
    focused.document_label = "DeltaVariant setup reference".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "DeltaVariant",
        &configure_how_focus_ir("DeltaVariant"),
        &[stale, focused],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("DeltaVariant setup reference"), "{answer}");
    assert!(answer.contains("delta-pay"), "{answer}");
    assert!(!answer.contains("AlphaSubject setup reference"), "{answer}");
    assert!(!answer.contains("alpha-subject"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_does_not_repeat_package_as_parameter() {
    let mut focused = evidence_chunk(
        1,
        Some("paragraph"),
        "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay",
    );
    focused.document_label = "DeltaVariant setup reference".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "DeltaVariant",
        &configure_how_focus_ir("DeltaVariant"),
        &[focused],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("delta-pay"), "{answer}");
    assert!(!answer.contains("- **parameter:**"), "{answer}");
}

#[test]
fn setup_configuration_anchor_candidate_uses_actionable_single_variant_directly() {
    let mut focused = evidence_chunk(
        1,
        Some("paragraph"),
        "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
    );
    focused.document_label = "DeltaVariant setup reference".to_string();
    let mut query_ir = configure_how_focus_ir("DeltaVariant");
    query_ir.target_types = vec![
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::Package,
        QueryTargetKind::Procedure,
    ];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "DeltaVariant".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];

    let candidate = build_setup_configuration_anchor_candidate(
        "DeltaVariant",
        &query_ir,
        std::slice::from_ref(&focused),
    )
    .expect("focused setup configuration candidate");

    assert!(
        candidate.should_use_as_direct_answer(&query_ir, &[focused]),
        "single focused setup variants with package/path anchors should not fall through to LLM"
    );
}

#[test]
fn setup_configuration_anchor_candidate_does_not_short_circuit_generic_procedure() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut focused = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Connector configuration\n\
             Settings are defined in /opt/subject/connector.conf in section [Main].",
    );
    focused.document_id = document_id;
    focused.revision_id = revision_id;
    focused.document_label = "Sample Connector setup guide".to_string();
    let mut parameter_row = evidence_chunk(
        2,
        Some("table_row"),
        "Sheet: Module settings | Row 12 | Name: endpoint | Type: URL | \
             Description: Service endpoint | Notes: Defaults to http://localhost",
    );
    parameter_row.document_id = document_id;
    parameter_row.revision_id = revision_id;
    parameter_row.document_label = focused.document_label.clone();
    let mut query_ir = configure_how_focus_ir("Sample Connector");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "Sample Connector".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];

    let candidate = build_setup_configuration_anchor_candidate(
        "How do I configure Sample Connector?",
        &query_ir,
        &[focused.clone(), parameter_row.clone()],
    )
    .expect("focused setup configuration candidate");

    assert!(
        !candidate
            .should_use_as_direct_answer(&query_ir, &[focused.clone(), parameter_row.clone()],),
        "a generic how-to needs ordered synthesis; parameter presence alone must not select the inventory renderer"
    );
    assert!(
        !candidate.should_use_as_preflight_answer(&query_ir, &[focused, parameter_row]),
        "evidence preflight must not reintroduce the same generic parameter dump"
    );
}

#[test]
fn setup_configuration_anchor_candidate_yields_mixed_remediation_ir_to_synthesis() {
    let mut focused = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Incident configuration\n\
             sample-configure incident-handler\n\
             Settings are defined in /opt/sample/incident.conf in section [Main].",
    );
    focused.document_label = "Sample Incident setup guide".to_string();
    let mut query_ir = configure_how_focus_ir("Sample Incident");
    query_ir.target_types = vec![
        QueryTargetKind::Procedure,
        QueryTargetKind::Remediation,
        QueryTargetKind::ErrorMessage,
        QueryTargetKind::ConfigurationFile,
    ];

    let candidate = build_setup_configuration_anchor_candidate(
        "How do I resolve the Sample Incident error?",
        &query_ir,
        std::slice::from_ref(&focused),
    )
    .expect("mixed remediation/configuration candidate");

    assert!(!candidate.should_use_as_direct_answer(&query_ir, std::slice::from_ref(&focused)));
    assert!(!candidate.should_use_as_preflight_answer(&query_ir, std::slice::from_ref(&focused)));
}

#[test]
fn setup_configuration_anchor_answer_aggregates_split_document_config_chunks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
    );
    anchor.document_id = document_id;
    anchor.revision_id = revision_id;
    anchor.document_label = "DeltaVariant setup reference".to_string();
    let mut parameters = evidence_chunk(
        2,
        Some("table_row"),
        "primaryKey = \"\"\ncredentialToken = \"\"\ntimeout = 60",
    );
    parameters.document_id = document_id;
    parameters.revision_id = revision_id;
    parameters.document_label = anchor.document_label.clone();

    let answer = build_setup_configuration_anchor_answer(
        "DeltaVariant",
        &configure_how_focus_ir("DeltaVariant"),
        &[anchor, parameters],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("/opt/subject/delta.conf"), "{answer}");
    assert!(answer.contains("DeltaVariant"), "{answer}");
    assert!(answer.contains("primaryKey"), "{answer}");
    assert!(answer.contains("credentialToken"), "{answer}");
    assert!(answer.contains("timeout"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_keeps_late_same_document_parameters() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "Workflow environment configuration\nsample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
    );
    anchor.document_id = document_id;
    anchor.revision_id = revision_id;
    anchor.document_label = "DeltaVariant setup reference".to_string();
    let parameter_text = (0..16)
        .map(|index| format!("field{index:02} = true"))
        .chain(std::iter::once("lateCredential = 42".to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    let mut parameters = evidence_chunk(2, Some("table_row"), &parameter_text);
    parameters.document_id = document_id;
    parameters.revision_id = revision_id;
    parameters.document_label = anchor.document_label.clone();

    let answer = build_setup_configuration_anchor_answer(
        "DeltaVariant",
        &configure_how_focus_ir("DeltaVariant"),
        &[anchor, parameters],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("lateCredential"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_preserves_split_command_only_anchor() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut command =
        evidence_chunk(1, Some("paragraph"), "sample-reconfigure --target=delta-subject");
    command.document_id = document_id;
    command.revision_id = revision_id;
    command.document_label = "DeltaVariant setup reference".to_string();
    let mut parameters = evidence_chunk(
        2,
        Some("table_row"),
        "Sheet: Module settings | Row 12 | Name: primaryKey | Type: string | \
             Description: Primary credential value",
    );
    parameters.document_id = document_id;
    parameters.revision_id = revision_id;
    parameters.document_label = command.document_label.clone();
    let mut query_ir = configure_how_focus_ir("DeltaVariant");
    query_ir.target_types = vec![
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::Parameter,
        QueryTargetKind::Procedure,
    ];

    let answer = build_setup_configuration_anchor_answer(
        "how to configure DeltaVariant?",
        &query_ir,
        &[parameters, command],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("`sample-reconfigure --target=delta-subject`"), "{answer}");
    assert!(answer.contains("`primaryKey`"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_preserves_structured_parameter_row_details() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "Environment Delta configuration\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
    );
    anchor.document_id = document_id;
    anchor.revision_id = revision_id;
    anchor.document_label = "DeltaVariant setup reference".to_string();
    let mut first_row = evidence_chunk(
        2,
        Some("table_row"),
        "Sheet: Module settings | Row 12 | Name: staticQrId | Type: string | \
             Description: Static QR identifier | Notes: Required for static QR workflows",
    );
    first_row.document_id = document_id;
    first_row.revision_id = revision_id;
    first_row.document_label = anchor.document_label.clone();
    let mut second_row = evidence_chunk(
        3,
        Some("table_row"),
        "Sheet: Module settings | Row 13 | Name: callbackUrl | Type: URL | \
             Description: Workflow status callback endpoint | Notes: Defaults to the service URL",
    );
    second_row.document_id = document_id;
    second_row.revision_id = revision_id;
    second_row.document_label = anchor.document_label.clone();

    let answer = build_setup_configuration_anchor_answer(
        "DeltaVariant",
        &configure_how_focus_ir("DeltaVariant"),
        &[anchor, first_row, second_row],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("**Parameter details:**"), "{answer}");
    assert!(answer.contains("`staticQrId`"), "{answer}");
    assert!(answer.contains("Static QR identifier"), "{answer}");
    assert!(answer.contains("Required for static QR workflows"), "{answer}");
    assert!(answer.contains("`callbackUrl`"), "{answer}");
    assert!(answer.contains("Workflow status callback endpoint"), "{answer}");
    assert!(answer.contains("Defaults to the service URL"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_prefers_exact_subject_over_richer_generic_anchor() {
    let mut generic = evidence_chunk(
        1,
        Some("paragraph"),
        "Node configuration\nsample-install alpha-node-tools\n\
             sample-configure alpha-node-tools\n\
             Settings are defined in /etc/alpha/node.conf in section [Agent].\n\
             endpoint = http://localhost\n\
             token = \"\"\n\
             retryCount = 3",
    );
    generic.document_label = "Alpha node configuration guide".to_string();
    let mut focused = evidence_chunk(
        2,
        Some("paragraph"),
        "Subject Beta workflow setup\n\
             Settings are defined in /opt/subject/beta/beta.ini in section [QrpBeta].\n\
             primaryKey = \"\"\n\
             credentialToken = \"\"",
    );
    focused.document_label = "Subject Beta setup reference".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "Beta",
        &configure_how_focus_ir("Beta"),
        &[generic, focused],
    )
    .expect("focused setup configuration answer");

    assert!(answer.contains("Subject Beta setup reference"), "{answer}");
    assert!(answer.contains("/opt/subject/beta/beta.ini"), "{answer}");
    assert!(!answer.contains("Alpha node configuration guide"), "{answer}");
    assert!(!answer.contains("alpha-node-tools"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_scores_label_identity_once_per_document() {
    let mut chunks = Vec::new();
    for index in 0..6 {
        let mut generic = evidence_chunk(
            index,
            Some("table_row"),
            "Sheet: Settings | Row 1 | Name: sharedTimeout | Type: integer",
        );
        generic.document_id = Uuid::from_u128(100);
        generic.document_label = "Subject Alpha setup reference".to_string();
        chunks.push(generic);
    }
    let mut exact = evidence_chunk(
        20,
        Some("paragraph"),
        "Module configuration\nsample-install beta-subject\n\
             sample-configure beta-subject\n\
             Settings are defined in /opt/subject/beta/beta.conf in section [QrpBeta].\n\
             primaryKey = \"\"",
    );
    exact.document_id = Uuid::from_u128(200);
    exact.document_label = "Subject Beta setup reference".to_string();
    chunks.push(exact);
    let mut query_ir = configure_how_focus_ir("Subject");
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Parameter];
    query_ir.target_entities = vec![
        crate::domains::query_ir::EntityMention {
            label: "Subject".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        },
        crate::domains::query_ir::EntityMention {
            label: "Beta".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        },
    ];

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Subject Beta?",
        &query_ir,
        &chunks,
    )
    .expect("focused setup configuration answer");

    let beta_position = answer.find("Subject Beta setup reference").expect(&answer);
    let alpha_position = answer.find("Subject Alpha setup reference").unwrap_or(usize::MAX);
    assert!(beta_position < alpha_position, "{answer}");
    assert!(answer.contains("/opt/subject/beta/beta.conf"), "{answer}");
    assert!(answer.contains("primaryKey"), "{answer}");
}

#[test]
fn setup_configuration_anchor_answer_skips_describe_parameter_inventory() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "Settings are defined in /x/s1-alpha.conf in section [Alpha].\n\
             firstValue = \"\"",
    );
    anchor.document_id = document_id;
    anchor.revision_id = revision_id;
    anchor.document_label = "S1 Alpha reference".to_string();
    let mut parameter_row = evidence_chunk(
        2,
        Some("table_row"),
        "Sheet: Parameters | Row 4 | Name: secondValue | Type: string | \
             Description: Secondary value | Notes: Optional",
    );
    parameter_row.document_id = document_id;
    parameter_row.revision_id = revision_id;
    parameter_row.document_label = anchor.document_label.clone();
    let mut query_ir = configure_how_focus_ir("S1");
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Parameter];

    assert!(
        build_setup_configuration_anchor_answer(
            "describe S1 values",
            &query_ir,
            &[anchor, parameter_row],
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_answer_keeps_section_only_variant() {
    let mut alpha = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\nsample-runner --install sample-unit\n\
             Settings are defined in /opt/subject/alpha/alpha.ini in section [AlphaQrp].\n\
             primaryKey = \"\"",
    );
    alpha.document_label = "Subject Alpha setup".to_string();
    let mut beta = evidence_chunk(
        2,
        Some("paragraph"),
        "Module configuration\n\
             Section [BetaQrp]\n\
             primaryKey = \"\"\n\
             timeout = 60",
    );
    beta.document_label = "Subject Beta setup".to_string();

    let answer = build_setup_configuration_anchor_answer(
        "how to configure Subject?",
        &configure_how_focus_ir("Subject"),
        &[alpha, beta],
    )
    .expect("setup variants answer");

    assert!(answer.contains("Subject Alpha setup"));
    assert!(answer.contains("Subject Beta setup"));
    assert!(answer.contains("BetaQrp"));
    assert!(answer.contains("timeout"));
}

#[test]
fn setup_configuration_anchor_answer_ignores_footnote_only_sections() {
    let mut notes = evidence_chunk(
        1,
        Some("paragraph"),
        "Subject overview\n\
             The integration has several deployment options [1].\n\
             See the operational note [Note] and the release marker [OK].\n\
             Additional background is available in appendix [2].",
    );
    notes.document_label = "Subject overview notes".to_string();

    assert!(
        build_setup_configuration_anchor_answer(
            "how to configure Subject?",
            &configure_how_focus_ir("Subject"),
            &[notes],
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_answer_skips_service_port_inventory_ir() {
    let mut first = evidence_chunk(
        1,
        Some("paragraph"),
        "[Service]\n\
             name = alpha-api\n\
             port = 8101",
    );
    first.document_label = "Alpha services".to_string();
    let mut second = evidence_chunk(
        2,
        Some("paragraph"),
        "[Service]\n\
             name = beta-worker\n\
             port = 8102",
    );
    second.document_label = "Beta services".to_string();
    let mut query_ir = configure_how_focus_ir("services");
    query_ir.act = QueryAct::Describe;
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::Service, QueryTargetKind::Port];

    assert!(
        build_setup_configuration_anchor_answer(
            "What service ports are configured?",
            &query_ir,
            &[first, second],
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_answer_skips_control_detail_question() {
    let mut query_ir = configure_how_focus_ir("Alpha and Beta threshold controls");
    query_ir.target_entities = vec![
        crate::domains::query_ir::EntityMention {
            label: "Alpha component".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        },
        crate::domains::query_ir::EntityMention {
            label: "Beta component".to_string(),
            role: crate::domains::query_ir::EntityRole::Object,
        },
    ];
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\n\
             Settings are defined in /opt/neutral/alpha.conf in section [Alpha].\n\
             thresholdLimit = 100",
    );
    chunk.document_label = "Alpha component reference".to_string();

    assert!(
            build_setup_configuration_anchor_answer(
                "How do Alpha and Beta implement threshold handling? What configuration controls the threshold?",
                &query_ir,
                &[chunk],
            )
            .is_none()
        );
}

#[test]
fn setup_configuration_anchor_answer_skips_unambiguous_versioned_procedure() {
    let mut ir = configure_how_focus_ir("Sample Target");
    ir.document_focus = None;
    ir.target_types =
        vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure, QueryTargetKind::Version];
    ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "Sample Target".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];

    let mut setup_chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\nsample-runner --install sample-unit\n\
             Settings are defined in /opt/subject/alpha/alpha.ini.",
    );
    setup_chunk.document_label = "Subject Alpha setup".to_string();

    assert!(
        build_setup_configuration_anchor_answer(
            "how to update Sample Target?",
            &ir,
            &[setup_chunk],
        )
        .is_none()
    );
}

#[test]
fn setup_configuration_anchor_answer_skips_typed_release_procedure_without_config_target() {
    let mut ir = configure_how_focus_ir("Sample Target");
    ir.document_focus = None;
    ir.target_types =
        vec![QueryTargetKind::Procedure, QueryTargetKind::Concept, QueryTargetKind::Release];
    ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "Sample Target".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    let mut setup_chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Module settings\nsample-runner --install sample-unit\n\
             Settings are defined in /opt/sample/sample.ini.\n\
             timeout = 30",
    );
    setup_chunk.document_label = "Sample Target setup note".to_string();

    assert!(
        build_setup_configuration_anchor_answer(
            "how to update Sample Target?",
            &ir,
            &[setup_chunk],
        )
        .is_none()
    );
}
