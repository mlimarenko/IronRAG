use super::test_support::*;
use super::*;

#[test]
fn render_primary_chunk_section_surfaces_setup_install_anchor_in_full() {
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\nsample-runner --install sample-link\nSettings are defined in the file /opt/alpha/connector/connector.conf",
    );
    anchor.document_label = "Sample Subject admin guide".to_string();
    anchor.score = Some(1.0);
    let mut dense_filler = evidence_chunk(
        2,
        Some("paragraph"),
        "Parameters: staticWidgetId credentialToken connectorPrimaryId currency widgetLifetime",
    );
    dense_filler.document_label = "Sample Subject admin guide".to_string();
    dense_filler.score = Some(9_999.0);
    let chunks = vec![dense_filler, anchor];

    let section = render_primary_chunk_section(
        "how to install and configure Sample Subject",
        &configure_how_focus_ir("Sample Subject"),
        &chunks,
        false,
    );

    assert!(section.contains("Setup install anchor"), "anchor section must be rendered");
    assert!(
        section.contains("sample-runner --install sample-link"),
        "install command must be present verbatim"
    );
}

#[test]
fn render_primary_chunk_section_skips_anchor_without_document_focus() {
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "sample-runner --install sample-link\nfile /opt/alpha/connector/connector.conf",
    );
    anchor.document_label = "Sample Subject admin guide".to_string();
    let chunks = vec![anchor];
    let mut query_ir = configure_how_focus_ir("Sample Subject");
    query_ir.document_focus = None;

    let section = render_primary_chunk_section(
        "how to install and configure Sample Subject",
        &query_ir,
        &chunks,
        false,
    );

    assert!(!section.contains("Setup install anchor"));
}

#[test]
fn render_primary_chunk_section_surfaces_setup_anchor_for_typed_setup_ir() {
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "Module configuration\nsample-runner --install sample-link\nSettings are defined in the file /opt/alpha/connector/connector.conf",
    );
    anchor.document_label = "Sample Subject admin guide".to_string();
    anchor.score = Some(1.0);
    let mut query_ir = configure_how_focus_ir("Sample Subject");
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.confidence = 0.25;
    query_ir.target_types = vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "Sample Subject".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir.document_focus = None;

    let section =
        render_primary_chunk_section("configure Sample Subject", &query_ir, &[anchor], false);

    assert!(section.contains("Setup install anchor"), "typed setup anchor must be rendered");
    assert!(
        section.contains("sample-runner --install sample-link"),
        "install command must remain in the prompt context"
    );
}

#[test]
fn render_primary_chunk_section_does_not_infer_setup_anchor_for_empty_ir() {
    let mut anchor = evidence_chunk(
        1,
        Some("paragraph"),
        "sample-runner --install sample-link\nfile /opt/alpha/connector/connector.conf",
    );
    anchor.document_label = "Sample Subject admin guide".to_string();
    let mut query_ir = configure_how_focus_ir("Sample Subject");
    query_ir.act = QueryAct::Describe;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.document_focus = None;
    query_ir.confidence = 0.2;

    let section =
        render_primary_chunk_section("configure Sample Subject", &query_ir, &[anchor], false);

    assert!(!section.contains("Setup install anchor"));
}

#[test]
fn render_primary_chunk_section_expands_typed_short_technical_rows() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = configure_how_focus_ir("Subject Alpha setup");
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.confidence = 0.25;
    query_ir.target_types = vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "QX".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir.document_focus = None;

    let mut chunks = vec![
        source_context_chunk(document_id, revision_id, 1, "QX alphaFlag = true"),
        source_context_chunk(document_id, revision_id, 2, "QX betaFlag = true"),
        source_context_chunk(document_id, revision_id, 3, "QX gammaFlag = true"),
        source_context_chunk(document_id, revision_id, 4, "QX deltaFlag = true"),
        source_context_chunk(document_id, revision_id, 5, "QX epsilonFlag = true"),
        source_context_chunk(document_id, revision_id, 6, "QX zetaFlag = true"),
        source_context_chunk(document_id, revision_id, 7, "QX etaFlag = true"),
        source_context_chunk(document_id, revision_id, 8, "QX thetaFlag = true"),
        source_context_chunk(document_id, revision_id, 9, "QX iotaFlag = true"),
        source_context_chunk(document_id, revision_id, 10, "QX kappaFlag = true"),
        source_context_chunk(document_id, revision_id, 11, "QX lambdaFlag = true"),
        source_context_chunk(document_id, revision_id, 12, "QX visibleMode = true"),
    ];
    for (rank, chunk) in chunks.iter_mut().enumerate() {
        chunk.document_label = "Subject Alpha setup".to_string();
        chunk.score = Some(100.0 - rank as f32);
    }

    let section = render_primary_chunk_section("QX settings", &query_ir, &chunks, false);

    assert!(
        section.contains("visibleMode"),
        "typed short-token structured rows below the old per-document cap must remain visible"
    );
}

#[test]
fn render_primary_chunk_section_keeps_late_setup_code_blocks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = configure_how_focus_ir("Subject Alpha setup");
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.confidence = 0.25;
    query_ir.target_types = vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "QX".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir.document_focus = None;

    let mut chunks = (1..=21)
        .map(|index| {
            source_context_chunk(
                document_id,
                revision_id,
                index,
                &format!("QX tableFlag{index} | boolean | true"),
            )
        })
        .collect::<Vec<_>>();
    let mut late_example =
        source_context_chunk(document_id, revision_id, 22, "[UI.Component]\nvisibleMode = true");
    late_example.chunk_kind = Some("code_block".to_string());
    chunks.push(late_example);

    let section = render_primary_chunk_section("QX settings", &query_ir, &chunks, false);

    assert!(
        section.contains("visibleMode = true"),
        "late code-block assignments retained by source-context retrieval must also be rendered for the answer model"
    );
}

#[test]
fn render_primary_chunk_section_keeps_late_structured_literals_in_plain_chunks() {
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        &format!(
            "{}\nlateStructuralKey = enabled\n/opt/sample/late.conf",
            (0..80)
                .map(|index| format!(
                    "background narrative line {index} about ordinary setup context"
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    chunk.document_label = "Sample Subject guide".to_string();
    chunk.score = Some(1.0);

    let section = render_primary_chunk_section(
        "how to configure Sample Subject",
        &configure_how_focus_ir("Sample Subject"),
        &[chunk],
        false,
    );

    assert!(section.contains("lateStructuralKey = enabled"), "{section}");
    assert!(section.contains("/opt/sample/late.conf"), "{section}");
}

#[test]
fn render_primary_chunk_section_keeps_late_salient_source_lines_in_plain_chunks() {
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        &format!(
            "{}\n2026-02-03: Theta marker changed from state K to state L.",
            (0..80)
                .map(|index| format!("background narrative line {index} about ordinary context"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    chunk.document_label = "Sample Subject notes".to_string();
    chunk.score = Some(1.0);
    let mut query_ir = configure_how_focus_ir("Sample Subject");
    query_ir.act = QueryAct::Describe;

    let section = render_primary_chunk_section(
        "describe recent entries for Sample Subject",
        &query_ir,
        &[chunk],
        false,
    );

    assert!(section.contains("Theta marker changed from state K to state L"), "{section}");
}

#[test]
fn deterministic_answer_keeps_salient_source_lines_out_of_visible_answer() {
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "2026-02-03: Theta marker changed from state K to state L.",
    );
    chunk.document_label = "Sample Subject notes".to_string();
    chunk.score = Some(1.0);
    let mut query_ir = configure_how_focus_ir("Sample Subject");
    query_ir.act = QueryAct::Describe;

    let answer = augment_deterministic_grounded_answer_with_evidence(
        "Sample Subject has recent recorded changes.".to_string(),
        "describe recent entries for Sample Subject",
        &query_ir,
        &[chunk],
    );

    assert!(!answer.contains("Theta marker changed from state K to state L"), "{answer}");
}

#[test]
fn deterministic_answer_keeps_focus_only_source_lines_out_of_visible_answer() {
    let mut chunk = evidence_chunk(1, Some("paragraph"), "Theta marker reached stable status");
    chunk.document_label = "Sample Subject notes".to_string();
    chunk.score = Some(1.0);
    let mut query_ir = configure_how_focus_ir("Sample Subject");
    query_ir.act = QueryAct::Describe;

    let answer = augment_deterministic_grounded_answer_with_evidence(
        "Sample Subject has notes.".to_string(),
        "describe Theta marker",
        &query_ir,
        &[chunk],
    );

    assert!(!answer.contains("Theta marker reached stable status"), "{answer}");
}

#[test]
fn configure_answers_keep_more_than_two_source_context_rows_per_document() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        source_context_chunk(
            document_id,
            revision_id,
            1,
            "Install package alpha-connector and edit /opt/alpha/connector.conf.",
        ),
        source_context_chunk(document_id, revision_id, 2, "| apiUrl | string | Service URL |"),
        source_context_chunk(
            document_id,
            revision_id,
            3,
            "| retryTimeout | integer | Response timeout | Default 10 |",
        ),
        source_context_chunk(
            document_id,
            revision_id,
            4,
            "| partnerId | string | Registered partner identifier |",
        ),
        source_context_chunk(
            document_id,
            revision_id,
            5,
            "| credentialToken | string | Shared authorization secret |",
        ),
    ];

    let section = render_primary_chunk_section(
        "How do I configure Subject Alpha parameters?",
        &configure_how_ir(),
        &chunks,
        false,
    );

    assert!(section.contains("apiUrl"));
    assert!(section.contains("retryTimeout"));
    assert!(section.contains("partnerId"));
    assert!(section.contains("credentialToken"));
}

#[test]
fn structured_source_unit_evidence_uses_extended_context() {
    let late_marker = "late-marker-structural-unit";
    let source_text = format!("[unit_id=a] {} {late_marker}", "content ".repeat(120));
    let chunk = evidence_chunk(7, Some(super::super::SOURCE_UNIT_CHUNK_KIND), &source_text);

    let lines = render_evidence_chunk_lines(&[&chunk], &[], "sampled");

    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("scope=source_unit"));
    assert!(lines[0].contains(late_marker));
}

#[test]
fn ordinary_evidence_chunks_remain_excerpt_bounded() {
    let late_marker = "late ordinary evidence marker";
    let source_text = format!("{} {late_marker}", "content ".repeat(120));
    let chunk = evidence_chunk(7, None, &source_text);

    let lines = render_evidence_chunk_lines(&[&chunk], &[], "sampled");

    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("scope=excerpt"));
    assert!(!lines[0].contains(late_marker));
}

#[test]
fn compare_context_reserves_a_structural_declaration_for_each_named_document() {
    let first_document_id = Uuid::now_v7();
    let second_document_id = Uuid::now_v7();
    let mut chunks = Vec::new();
    for (document_id, label, declaration) in [
        (
            first_document_id,
            "alpha_state_engine.rs",
            "enum AlphaFault {\n    InvalidTransition,\n}",
        ),
        (second_document_id, "beta_data_flow.py", "class BetaFault(Exception):\n    pass"),
    ] {
        for index in 0..2 {
            let mut filler = evidence_chunk(
                index,
                Some("paragraph"),
                "error handling patterns types comparison_timeout = 30 comparison_retries = 4",
            );
            filler.document_id = document_id;
            filler.document_label = label.to_string();
            filler.score = Some(100.0 - index as f32);
            chunks.push(filler);
        }
        let mut declaration_chunk = evidence_chunk(3, Some("code_block"), declaration);
        declaration_chunk.document_id = document_id;
        declaration_chunk.document_label = label.to_string();
        declaration_chunk.score = Some(1.0);
        chunks.push(declaration_chunk);
    }
    let mut query_ir = configure_how_ir();
    query_ir.act = QueryAct::Compare;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.target_entities = vec![
        crate::domains::query_ir::EntityMention {
            label: "Alpha State Engine".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        },
        crate::domains::query_ir::EntityMention {
            label: "Beta Data Flow".to_string(),
            role: crate::domains::query_ir::EntityRole::Object,
        },
    ];
    query_ir.comparison = Some(crate::domains::query_ir::ComparisonSpec {
        a: Some("Alpha State Engine".to_string()),
        b: Some("Beta Data Flow".to_string()),
        dimension: "error types".to_string(),
    });

    let section = render_primary_chunk_section(
        "Compare Alpha State Engine and Beta Data Flow error types.",
        &query_ir,
        &chunks,
        false,
    );

    assert!(section.contains("enum AlphaFault"), "{section}");
    assert!(section.contains("class BetaFault"), "{section}");
}
