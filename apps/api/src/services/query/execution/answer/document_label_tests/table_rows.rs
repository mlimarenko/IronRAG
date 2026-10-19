use super::*;

#[test]
fn build_table_row_grounded_answer_supports_normalized_row_tokens() {
    let document_id = Uuid::now_v7();
    let chunks = (1..=5)
        .map(|row_number| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0 - row_number as f32),
            source_text: format!("Sheet: test1 | Row {row_number} | col_1: {row_number}"),
        })
        .collect::<Vec<_>>();

    let ir = initial_table_rows_ir(5);

    assert_eq!(
        build_table_row_grounded_answer("Show the first 5 rows from sample-heavy-1.xls.", Some(&ir), &chunks),
        Some(
            "- Row 1: col_1 = `1`\n- Row 2: col_1 = `2`\n- Row 3: col_1 = `3`\n- Row 4: col_1 = `4`\n- Row 5: col_1 = `5`"
                .to_string()
        )
    );
}

#[test]
fn build_table_row_grounded_answer_matches_requested_headers() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "organizations-100.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: organizations-100 | Row 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "organizations-100.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: organizations-100 | Row 2 | Name: Acme Holdings | Country: Canada | Industry: Logistics"
                    .to_string(),
        },
    ];

    assert_eq!(
        build_table_row_grounded_answer(
            "In organizations-100.csv, what Country and Industry does Ferrell LLC have?",
            None,
            &chunks,
        ),
        Some("Name: `Ferrell LLC`; Country: `Papua New Guinea`; Industry: `Plastics`".to_string())
    );
}

#[test]
fn build_table_row_grounded_answer_matches_camel_case_requested_headers() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "sales.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Revenue | Table: RevenueTable | Row 3 | Quarter: Q3 | RevenueUSD: 143500 | Region: Central | RiskFlag: high | OwnerTeam: Gamma Team"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "sales.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Revenue | Table: RevenueTable | Row 4 | Quarter: Q4 | RevenueUSD: 160000 | Region: West | RiskFlag: low | OwnerTeam: Beta Team"
                    .to_string(),
        },
    ];

    assert_eq!(
        build_table_row_grounded_answer(
            "In the revenue workbook, which region has Q3 revenue of 143500 USD, and what risk flag is listed?",
            None,
            &chunks,
        ),
        Some("RevenueUSD: `143500`; Region: `Central`; RiskFlag: `high`".to_string())
    );
}

#[test]
fn build_table_row_grounded_answer_matches_partial_compound_header_and_row_identifier() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "matrix.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Matrix | Row 4 | GroupName: Group Delta | WindowValue: 45 units | RatePercent: 12 | RequiredReviewer: Team Q"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "matrix.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Matrix | Row 5 | GroupName: Group Gamma | WindowValue: 30 units | RatePercent: 9 | RequiredReviewer: Team R"
                    .to_string(),
        },
    ];

    assert_eq!(
        build_table_row_grounded_answer(
            "What rate percent and reviewer are listed for Group Delta records?",
            None,
            &chunks,
        ),
        Some("GroupName: `Group Delta`; RatePercent: `12`; RequiredReviewer: `Team Q`".to_string())
    );
}

#[test]
fn build_table_row_grounded_answer_preserves_raw_pipe_row_identifier() {
    let document_id = Uuid::now_v7();
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("table_row".to_string()),
        document_id,
        document_label: "matrix.xlsx".to_string(),
        excerpt: String::new(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(10.0),
        source_text: "| Group Delta | 30 units | 7 | Reviewer Q |".to_string(),
    }];

    assert_eq!(
        build_table_row_grounded_answer(
            "What number and reviewer are listed for Group Delta in the workbook?",
            None,
            &chunks,
        ),
        Some("`Group Delta`; `30 units`; `7`; `Reviewer Q`".to_string())
    );
}

#[test]
fn build_table_row_grounded_answer_handles_focused_row_lookup_without_inventory_intent() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "operations.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Operations | Table: StatusTable | Row 2 | StatusCode: amber | Region: North | OwnerTeam: Delta"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "operations.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Operations | Table: StatusTable | Row 3 | StatusCode: green | Region: South | OwnerTeam: Alpha"
                    .to_string(),
        },
    ];
    let ir = QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableRow],
        ..describe_query_ir()
    };

    assert_eq!(
        build_table_row_grounded_answer(
            "For the amber status row, what region and owner team are listed?",
            Some(&ir),
            &chunks,
        ),
        Some("StatusCode: `amber`; Region: `North`; OwnerTeam: `Delta`".to_string())
    );
}

#[test]
fn build_table_row_grounded_answer_lists_values_for_targeted_single_value_sheets() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "sample-simple-2.xls".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: "Sheet: test1 | Row 1 | col_1: test1".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "sample-simple-2.xls".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text: "Sheet: test2 | Row 1 | col_1: test2".to_string(),
        },
    ];

    let ir = table_row_inventory_ir();

    assert_eq!(
        build_table_row_grounded_answer(
            "List the values in sample-simple-2.xls.",
            Some(&ir),
            &chunks
        ),
        Some("- test1 row 1: `test1`\n- test2 row 1: `test2`".to_string())
    );
}

#[test]
fn build_table_row_grounded_answer_lists_values_for_table_row_enumeration_ir() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "extensions.xlsb".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Sheet1 | Row 1 | ID: 1 | Type: PNG | Description: Portable Network Graphics"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "extensions.xlsb".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Sheet1 | Row 2 | ID: 2 | Type: GIF | Description: Graphics Interchange Format"
                    .to_string(),
        },
    ];
    let ir = table_row_inventory_ir();

    assert_eq!(
        build_table_row_grounded_answer(
            "List rows in extensions.xlsb.",
            Some(&ir),
            &chunks
        ),
        Some(
            "- Sheet1 row 1: ID = `1`, Type = `PNG`, Description = `Portable Network Graphics`\n- Sheet1 row 2: ID = `2`, Type = `GIF`, Description = `Graphics Interchange Format`"
                .to_string()
        )
    );
}

#[test]
fn parse_table_row_chunk_keeps_post_row_table_and_row_fields_as_data() {
    let chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "schema.pdf".to_string(),
        excerpt: String::new(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(10.0),
        source_text:
            "Sheet: Schema | Table: Key Indexes | Row 1 | Table: accounts | Column: LOWER(email) | Row Label: email index"
                .to_string(),
    };

    let parsed = parse_table_row_chunk(&chunk).expect("table row should parse");

    assert_eq!(parsed.sheet_name, "Schema");
    assert_eq!(parsed.table_name.as_deref(), Some("Key Indexes"));
    assert_eq!(parsed.row_number, 1);
    assert!(parsed.fields.iter().any(|(header, value)| header == "Table" && value == "accounts"));
    assert!(
        parsed.fields.iter().any(|(header, value)| header == "Column" && value == "LOWER(email)")
    );
    assert!(
        parsed.fields.iter().any(|(header, value)| header == "Row Label" && value == "email index")
    );
}
