use super::*;

#[test]
fn query_focused_source_rows_select_late_matching_chunk_inside_selected_document() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(document_id, revision_id, 0, "paragraph", "Alpha service overview"),
        chunk_row(
            document_id,
            revision_id,
            4,
            "paragraph",
            "RareNeedle setting controls the payment confirmation format.",
        ),
        chunk_row(document_id, revision_id, 5, "paragraph", "Unrelated appendix"),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_query_focused_source_rows(
        &rows,
        &["rareneedle".to_string(), "payment".to_string()],
        false,
        &anchors,
        1,
    );

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, 4);
}

#[test]
fn path_source_rows_select_path_literals_inside_selected_document() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(document_id, revision_id, 0, "paragraph", "Alpha service overview"),
        chunk_row(
            document_id,
            revision_id,
            3,
            "paragraph",
            "The connector parameters are stored in /opt/provider-alpha/connector.conf.",
        ),
        chunk_row(document_id, revision_id, 4, "paragraph", "Unrelated appendix"),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_path_source_rows(&rows, &anchors, 1, false);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, 3);
}

#[test]
fn path_source_rows_skip_existing_anchor_chunks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(
            document_id,
            revision_id,
            0,
            "paragraph",
            "The connector parameters are stored in /opt/provider-alpha/connector.conf.",
        ),
        chunk_row(
            document_id,
            revision_id,
            2,
            "paragraph",
            "The audit output is written to /var/log/provider-alpha/audit.log.",
        ),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_path_source_rows(&rows, &anchors, 1, false);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, 2);
}

#[test]
fn configuration_path_source_rows_prefer_module_setup_commands() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(document_id, revision_id, 0, "paragraph", "Alpha service overview"),
        chunk_row(
            document_id,
            revision_id,
            1,
            "code_block",
            "Install the module:\nsample-install alpha-connector\n\nConfigure it:\nsample-configure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf.",
        ),
        chunk_row(
            document_id,
            revision_id,
            8,
            "code_block",
            "Example paths: /opt/alpha/ui.ini /opt/alpha/display.ini /opt/alpha/log.ini.",
        ),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_path_source_rows(&rows, &anchors, 1, true);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, 1);
}

#[test]
fn configuration_path_source_rows_keep_setup_anchor_chunks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(
            document_id,
            revision_id,
            1,
            "code_block",
            "Install the module:\nsample-install alpha-connector\n\nConfigure it:\nsample-configure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf.",
        ),
        chunk_row(
            document_id,
            revision_id,
            2,
            "table_row",
            "| url | string | Server URL | Default http://localhost |",
        ),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 1, score: 10.0, first_rank: 0 }];

    let selected = select_path_source_rows(&rows, &anchors, 1, true);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, 1);
}

#[test]
fn procedural_structured_siblings_select_rows_after_anchors_without_query_terms() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=5]"),
        chunk_row(
            document_id,
            revision_id,
            1,
            "code_block",
            "install package alpha-connector\nconfig /opt/alpha/connector.conf",
        ),
        chunk_row(document_id, revision_id, 2, "table_row", "merchantId | partner id"),
        chunk_row(document_id, revision_id, 3, "paragraph", "Narrative detail"),
        chunk_row(document_id, revision_id, 4, "table_row", "timeout | request timeout"),
        chunk_row(document_id, revision_id, 19, "table_row", "lateParam | out of window"),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 2, 4]);
}

#[test]
fn procedural_structured_siblings_include_literal_paragraphs_after_anchors() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=5]"),
        chunk_row(document_id, revision_id, 1, "paragraph", "[UI.Alpha.Form]\nalphaFlag = true"),
        chunk_row(document_id, revision_id, 2, "paragraph", "betaMode = 1"),
        chunk_row(document_id, revision_id, 3, "paragraph", "alpha beta gamma"),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 2]);
}

#[test]
fn procedural_structured_siblings_skip_empty_metadata_blocks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(document_id, revision_id, 1, "metadata_block", ""),
        chunk_row(document_id, revision_id, 2, "table_row", "alphaFlag | boolean | true"),
        chunk_row(document_id, revision_id, 3, "code_block", "[Main]\nalphaFlag = true"),
    ];
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [2, 3]);
}

#[test]
fn procedural_structured_siblings_reserve_setup_rows_before_late_anchor_rows() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=8]"),
        chunk_row(
            document_id,
            revision_id,
            1,
            "code_block",
            "install package alpha-connector\nconfig /opt/alpha/connector.conf",
        ),
        chunk_row(document_id, revision_id, 4, "table_row", "merchantId | partner id"),
        chunk_row(document_id, revision_id, 21, "table_row", "lateFlag | optional behavior"),
        chunk_row(document_id, revision_id, 22, "table_row", "lateMode | optional mode"),
    ];
    let anchors = vec![
        SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 },
        SourceContextAnchor { chunk_index: 20, score: 9.0, first_rank: 1 },
    ];

    let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 4, 21]);
}

#[test]
fn procedural_structured_siblings_do_not_starve_late_example_blocks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut rows =
        vec![chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=32]")];
    rows.extend((1..=10).map(|index| {
        chunk_row(
            document_id,
            revision_id,
            index,
            "table_row",
            &format!("headParam{index} | value"),
        )
    }));
    rows.extend([
        chunk_row(document_id, revision_id, 20, "heading", "Detailed option group"),
        chunk_row(document_id, revision_id, 21, "table_row", "lateFlag | boolean | true"),
        chunk_row(document_id, revision_id, 22, "code_block", "[Main]\nlateFlag = true"),
        chunk_row(document_id, revision_id, 23, "table_row", "printSlip | boolean | false"),
        chunk_row(document_id, revision_id, 24, "code_block", "[Check]\nprintSlip = false"),
        chunk_row(document_id, revision_id, 25, "table_row", "visible | boolean | true"),
        chunk_row(document_id, revision_id, 26, "code_block", "[UI.Component]\nvisible = true"),
    ]);
    let anchors = vec![
        SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 },
        SourceContextAnchor { chunk_index: 20, score: 9.0, first_rank: 1 },
    ];

    let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 12);
    let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

    assert!(selected_indexes.contains(&24), "{selected_indexes:?}");
    assert!(selected_indexes.contains(&26), "{selected_indexes:?}");
}

#[test]
fn table_structured_siblings_keep_head_table_rows_without_setup_cap() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut head =
        chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=12]");
    head.section_path = vec!["schema".to_string(), "accounts".to_string()];
    let mut rows = vec![head];
    rows.extend((1..=8).map(|index| {
        let mut row = chunk_row(
            document_id,
            revision_id,
            index,
            "table_row",
            &format!("Sheet: Schema | Table: 1. Table: accounts | Row {index} | Column: c{index}"),
        );
        row.section_path = vec!["schema".to_string(), "accounts".to_string()];
        row
    }));
    let mut other_table_row = chunk_row(
        document_id,
        revision_id,
        9,
        "table_row",
        "Sheet: Schema | Table: 2. Table: orders | Row 1 | Column: order_id",
    );
    other_table_row.section_path = vec!["schema".to_string(), "orders".to_string()];
    rows.push(other_table_row);
    let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

    let selected = select_table_structured_sibling_rows(&rows, &anchors, 16);

    assert_eq!(
        selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6, 7, 8]
    );
}

#[test]
fn table_structured_siblings_include_section_heading_for_table_rows() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut heading = chunk_row(document_id, revision_id, 0, "heading", "## Table: records");
    heading.section_path = vec!["schema".to_string(), "records".to_string()];
    let mut rows = vec![heading];
    rows.extend((1..=4).map(|index| {
        let mut row = chunk_row(
            document_id,
            revision_id,
            index,
            "table_row",
            &format!("| field_{index} | text | description |"),
        );
        row.section_path = vec!["schema".to_string(), "records".to_string()];
        row
    }));
    let mut other_heading = chunk_row(document_id, revision_id, 10, "heading", "## Table: events");
    other_heading.section_path = vec!["schema".to_string(), "events".to_string()];
    let mut other_row =
        chunk_row(document_id, revision_id, 11, "table_row", "| event_id | text | description |");
    other_row.section_path = vec!["schema".to_string(), "events".to_string()];
    rows.extend([other_heading, other_row]);
    let anchors = vec![SourceContextAnchor { chunk_index: 2, score: 10.0, first_rank: 0 }];

    let selected = select_table_structured_sibling_rows(&rows, &anchors, 16);

    assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
}

#[test]
fn procedural_structured_sibling_windows_expand_forward_only() {
    let anchors = vec![
        SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 },
        SourceContextAnchor { chunk_index: 20, score: 9.0, first_rank: 1 },
    ];

    assert_eq!(
        procedural_structured_sibling_windows(
            &anchors,
            SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD
        ),
        vec![
            (0, SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD),
            (20, 20 + SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD)
        ]
    );
}

#[test]
fn source_context_focus_keywords_include_typed_query_ir_focus() {
    let mut query_ir = QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: vec![crate::domains::query_ir::EntityMention {
            label: "deferred ticket".to_string(),
            role: crate::domains::query_ir::EntityRole::Object,
        }],
        literal_constraints: vec![crate::domains::query_ir::LiteralSpan {
            text: "code verification".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Other,
        }],
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(crate::domains::query_ir::DocumentHint {
            hint: "regulated product category".to_string(),
        }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    };

    let keywords = source_context_focus_keywords(
        "Which controlled product rule applies?",
        Some(&query_ir),
        &["controlled".to_string()],
    );

    assert!(keywords.contains(&"regulated product category".to_string()));
    assert!(keywords.contains(&"deferred ticket".to_string()));
    assert!(keywords.contains(&"code verification".to_string()));
    assert!(keywords.contains(&"controlled".to_string()));

    query_ir.document_focus = None;
    let keywords_without_focus =
        source_context_focus_keywords("Which rule applies?", Some(&query_ir), &[]);
    assert!(!keywords_without_focus.contains(&"regulated product category".to_string()));
}

#[test]
fn neighbor_score_stays_below_anchor_score() {
    let score = source_context_neighbor_score(42.0, 0.0, 10);

    assert!(score < 42.0);
}

#[test]
fn focused_match_extends_neighbor_windows() {
    let span = source_context_neighbor_span(None);
    let mut anchors = vec![
        SourceContextAnchor { chunk_index: 24, score: 12.0, first_rank: 0 },
        SourceContextAnchor { chunk_index: 22, score: 11.0, first_rank: 1 },
    ];

    push_unique_source_context_anchor(
        &mut anchors,
        SourceContextAnchor { chunk_index: 20, score: 13.0, first_rank: usize::MAX },
    );

    assert_eq!(anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>(), [24, 22, 20]);
    assert!(source_context_neighbor_windows(&anchors, span).contains(&(19, 21)));
    assert!(
        source_context_best_neighbor_score(&anchors, 19, span).is_some(),
        "the source chunk immediately before a focused match must be eligible"
    );
}

#[test]
fn focused_match_anchor_dedupes_against_existing_anchor() {
    let mut anchors = vec![SourceContextAnchor { chunk_index: 7, score: 5.0, first_rank: 4 }];

    push_unique_source_context_anchor(
        &mut anchors,
        SourceContextAnchor { chunk_index: 7, score: 8.0, first_rank: usize::MAX },
    );

    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].chunk_index, 7);
    assert_eq!(anchors[0].score, 8.0);
}
