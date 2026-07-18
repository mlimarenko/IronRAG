use std::collections::HashMap;

use uuid::Uuid;

use crate::domains::query_ir::{
    EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope, SourceSliceDirection,
    SourceSliceFilter, SourceSliceSpec,
};
use crate::infra::knowledge_rows::KnowledgeDocumentRow;
use crate::services::query::effective_query::{
    EFFECTIVE_QUERY_QUESTION_PREFIX, EFFECTIVE_QUERY_SCOPE_PREFIX,
};
use crate::services::query::execution::types::RuntimeMatchedChunk;
use crate::shared::extraction::table_summary::{
    build_table_column_summaries, render_table_column_summary,
};

use super::super::{
    build_missing_explicit_document_answer, build_table_row_grounded_answer,
    build_table_summary_grounded_answer, concise_document_subject_label,
    document_focus_marker_hits, focused_answer_document_id, parse_table_row_chunk,
    question_asks_table_aggregation, render_table_summary_chunk_section,
};

fn effective_query_text(scope: &str, question: &str) -> String {
    format!("{EFFECTIVE_QUERY_SCOPE_PREFIX} {scope}\n{EFFECTIVE_QUERY_QUESTION_PREFIX} {question}")
}

fn describe_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.0,
    }
}

fn table_row_inventory_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableRow],
        ..describe_query_ir()
    }
}

fn initial_table_rows_ir(row_count: u16) -> QueryIR {
    QueryIR {
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableRow],
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Head,
            count: Some(row_count),
            filter: SourceSliceFilter::None,
        }),
        ..describe_query_ir()
    }
}

fn table_average_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableAverage],
        ..describe_query_ir()
    }
}

fn table_frequency_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableFrequency],
        ..describe_query_ir()
    }
}

fn table_column_inventory_ir(label: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::TableRow,
            crate::domains::query_ir::QueryTargetKind::TableSummary,
        ],
        target_entities: vec![EntityMention {
            label: label.to_string(),
            role: EntityRole::Subject,
        }],
        ..describe_query_ir()
    }
}

fn retrieve_table_column_inventory_ir(label: &str) -> QueryIR {
    QueryIR { act: QueryAct::RetrieveValue, ..table_column_inventory_ir(label) }
}

#[test]
fn table_summary_with_table_row_target_is_not_aggregation_lookup() {
    let row_lookup_ir = retrieve_table_column_inventory_ir("accounts table");
    let summary_ir = QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableSummary],
        ..describe_query_ir()
    };

    assert!(!question_asks_table_aggregation("", Some(&row_lookup_ir)));
    assert!(question_asks_table_aggregation("", Some(&summary_ir)));
}

#[test]
fn concise_document_subject_label_strips_spreadsheet_extensions() {
    assert_eq!(
        concise_document_subject_label("spreadsheet_ODS_API_reference.xlsb"),
        "Spreadsheet ODS API reference"
    );
    assert_eq!(
        concise_document_subject_label("spreadsheet_ods_api_reference.xlsb"),
        "Spreadsheet ods api reference"
    );
    assert_eq!(concise_document_subject_label("inventory_snapshot.ods"), "Inventory snapshot");
}

#[test]
fn concise_document_subject_label_preserves_explicit_unicode_casing() {
    assert_eq!(concise_document_subject_label("phase_ΔΣ_overview.md"), "Phase ΔΣ overview");
}

#[test]
fn concise_document_subject_label_does_not_infer_acronyms_from_lowercase() {
    assert_eq!(concise_document_subject_label("rag_overview.md"), "Rag overview");
}

#[test]
fn concise_document_subject_label_does_not_strip_named_suffixes() {
    assert_eq!(concise_document_subject_label("sample_wikipedia.md"), "Sample wikipedia");
}

#[test]
fn document_focus_marker_hits_distinguishes_xls_from_xlsx() {
    assert_eq!(
        document_focus_marker_hits("What does inventory.xlsx validate?", "inventory.xlsx",),
        1
    );
    assert_eq!(
        document_focus_marker_hits("What does inventory.xlsx validate?", "inventory.xls",),
        0
    );
    assert_eq!(
        document_focus_marker_hits("What does inventory.xls validate?", "inventory.xls",),
        1
    );
}

#[test]
fn focused_answer_document_id_prefers_explicit_extension_match() {
    let csv_id = Uuid::now_v7();
    let xlsx_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: csv_id,
            document_label: "records-a.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "Sheet: records-a | Row 1 | Email: record-a@example.invalid".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: xlsx_id,
            document_label: "records-a.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: "Sheet: records-a | Row 1 | Email: record-a@example.invalid".to_string(),
        },
    ];

    assert_eq!(
        focused_answer_document_id(
            "In records-a.csv what is sample record's job title (email record-a@example.invalid)?",
            &chunks,
        ),
        Some(csv_id)
    );
}

#[test]
fn focused_answer_document_id_prefers_current_question_segment() {
    let focused_id = Uuid::now_v7();
    let stale_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: stale_id,
            document_label: "Sample Unit Admin Guide".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: String::new(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: None,
            document_id: focused_id,
            document_label: "Sample Unit".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: String::new(),
        },
    ];

    assert_eq!(
        focused_answer_document_id(
            &effective_query_text(
                "Prior assistant listed Sample Unit Admin Guide.",
                "Sample Unit setup",
            ),
            &chunks,
        ),
        Some(focused_id)
    );
}

#[test]
fn build_table_row_grounded_answer_supports_canonical_row_tokens() {
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
    let chunks = vec![RuntimeMatchedChunk {
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
    }];

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
    let chunks = vec![RuntimeMatchedChunk {
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
    }];

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
        source_text:
            "Sheet: Matrix | Row 4 | GroupName: Group Delta | WindowValue: 45 units | RatePercent: 12 | RequiredReviewer: Team Q"
                .to_string(),
    }];

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
    let chunks = vec![RuntimeMatchedChunk {
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
    }];
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

#[test]
fn build_table_row_grounded_answer_lists_schema_columns_for_target_table() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 1 | Column: account_id | Type: UUID | Constraints: PRIMARY KEY | Description: Unique account identifier"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 2 | Column: email | Type: VARCHAR(255) | Constraints: UNIQUE, NOT NULL | Description: Login email"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 2,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(8.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 3 | Column: status | Type: VARCHAR(20) | Constraints: NOT NULL | Description: Account state"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 3,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(99.0),
            source_text:
                "Sheet: Store Schema | Table: 6. Key Indexes | Row 1 | Index Name: idx_accounts_email_lower | Table: accounts | Column: LOWER(email) | Type: UNIQUE, btree"
                    .to_string(),
        },
    ];
    let ir = table_column_inventory_ir("accounts table");

    let answer = build_table_row_grounded_answer(
        "What columns does the accounts table have?",
        Some(&ir),
        &chunks,
    )
    .expect("schema column inventory answer");

    assert!(answer.contains("`account_id`"));
    assert!(answer.contains("`email`"));
    assert!(answer.contains("`status`"));
    assert!(answer.contains("`UUID`"));
    assert!(!answer.contains("LOWER(email)"));
}

#[test]
fn build_table_row_grounded_answer_does_not_list_schema_columns_without_table_ir_targets() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 1 | Column: account_id | Type: UUID | Constraints: PRIMARY KEY | Description: Unique account identifier"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 2 | Column: email | Type: VARCHAR(255) | Constraints: UNIQUE, NOT NULL | Description: Login email"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 2,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(99.0),
            source_text:
                "Sheet: Store Schema | Table: 6. Key Indexes | Row 1 | Index Name: idx_accounts_email_lower | Table: accounts | Column: LOWER(email) | Type: UNIQUE, btree"
                    .to_string(),
        },
    ];
    let ir = describe_query_ir();

    let answer = build_table_row_grounded_answer(
        "What columns does the accounts table have?",
        Some(&ir),
        &chunks,
    );

    if let Some(answer) = answer.as_deref() {
        assert!(!answer.contains("`account_id`"), "{answer}");
        assert!(!answer.contains("`UUID`"), "{answer}");
    }
}

#[test]
fn build_table_row_grounded_answer_lists_schema_columns_for_retrieve_value_ir() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 1 | Column: account_id | Type: UUID | Constraints: PRIMARY KEY | Description: Unique account identifier"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 2 | Column: email | Type: VARCHAR(255) | Constraints: UNIQUE, NOT NULL | Description: Login email"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 2,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(99.0),
            source_text:
                "Sheet: Store Schema | Table: 6. Key Indexes | Row 1 | Index Name: idx_accounts_email_lower | Table: accounts | Column: LOWER(email) | Type: UNIQUE, btree"
                    .to_string(),
        },
    ];
    let ir = retrieve_table_column_inventory_ir("accounts table");

    let answer = build_table_row_grounded_answer(
        "Which columns are in the accounts table?",
        Some(&ir),
        &chunks,
    )
    .expect("schema column inventory answer");

    assert!(answer.contains("`account_id`"));
    assert!(answer.contains("`email`"));
    assert!(answer.contains("`UUID`"));
    assert!(!answer.contains("LOWER(email)"));
}

#[test]
fn build_table_row_grounded_answer_lists_raw_pipe_columns_from_heading_section() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: Some("heading".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: "## 1. Table: accounts".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 1,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text: "| account_id | UUID | PRIMARY KEY | Unique account identifier |"
                .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 2,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(8.0),
            source_text: "| email | VARCHAR(255) | UNIQUE, NOT NULL | Login email |".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 3,
            chunk_kind: Some("heading".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(20.0),
            source_text: "## 2. Table: orders".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 4,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(20.0),
            source_text: "| order_id | UUID | PRIMARY KEY | Unique order identifier |".to_string(),
        },
    ];
    let ir = retrieve_table_column_inventory_ir("accounts table");

    let answer = build_table_row_grounded_answer(
        "Which columns are in the accounts table?",
        Some(&ir),
        &chunks,
    )
    .expect("raw pipe column inventory answer");

    assert!(answer.contains("`account_id`"));
    assert!(answer.contains("`email`"));
    assert!(answer.contains("`UUID`"));
    assert!(!answer.contains("order_id"));
}

#[test]
fn build_table_row_grounded_answer_rejects_unmatched_column_inventory_rows() {
    let document_id = Uuid::now_v7();
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "implementation-notes.md".to_string(),
        excerpt: String::new(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(99.0),
        source_text:
            "Sheet: Notes | Table: Route Notes | Row 1 | Column: Application route handlers | Detail: Handler list"
                .to_string(),
    }];
    let ir = QueryIR { act: QueryAct::Enumerate, ..table_column_inventory_ir("accounts table") };

    assert_eq!(
        build_table_row_grounded_answer(
            "Which columns are in the accounts table?",
            Some(&ir),
            &chunks,
        ),
        None
    );
}

#[test]
fn build_table_summary_grounded_answer_reports_most_frequent_values() {
    let document_id = Uuid::now_v7();
    let summaries = build_table_column_summaries(
        Some("organizations-100"),
        None,
        &["Country".to_string(), "Industry".to_string()],
        &[
            vec!["Sweden".to_string(), "Plastics".to_string()],
            vec!["Benin".to_string(), "Plastics".to_string()],
            vec!["Sweden".to_string(), "Printing".to_string()],
            vec!["Benin".to_string(), "Printing".to_string()],
        ],
    );
    let chunks = summaries
        .into_iter()
        .enumerate()
        .map(|(index, summary)| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "organizations-100.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0 - index as f32),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "What is the most frequent Country in organizations-100.csv?",
            Some(&table_frequency_ir()),
            &chunks,
        ),
        Some(
            "The most frequent `Country` values are `Benin`, `Sweden` (`2` rows each).".to_string()
        )
    );
}

#[test]
fn build_table_summary_grounded_answer_reports_no_single_most_frequent_value() {
    let document_id = Uuid::now_v7();
    let summaries = build_table_column_summaries(
        Some("records-b"),
        None,
        &["City".to_string()],
        &[vec!["Moscow".to_string()], vec!["London".to_string()], vec!["Berlin".to_string()]],
    );
    let chunks = summaries
        .into_iter()
        .map(|summary| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "records-b.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "What is the most frequent City in records-b.csv?",
            Some(&table_frequency_ir()),
            &chunks,
        ),
        Some(
            "There is no single most frequent `City` value: every value appears once.".to_string()
        )
    );
}

#[test]
fn build_table_summary_grounded_answer_reports_average_values() {
    let document_id = Uuid::now_v7();
    let summaries = build_table_column_summaries(
        Some("products-100"),
        None,
        &["Stock".to_string()],
        &[vec!["100".to_string()], vec!["200".to_string()], vec!["300".to_string()]],
    );
    let chunks = summaries
        .into_iter()
        .map(|summary| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "products-100.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "What is the average Stock in products-100.csv?",
            Some(&table_average_ir()),
            &chunks,
        ),
        Some("The average `Stock` is `200` across `3` rows.".to_string())
    );
}

#[test]
fn build_table_summary_grounded_answer_reports_average_number_of_employees() {
    let document_id = Uuid::now_v7();
    let summaries = build_table_column_summaries(
        Some("organizations-100"),
        None,
        &["Number of Employees".to_string()],
        &[vec!["10".to_string()], vec!["20".to_string()]],
    );
    let chunks = summaries
        .into_iter()
        .map(|summary| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "organizations-100.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "What is the average Number of Employees in organizations-100.csv?",
            Some(&table_average_ir()),
            &chunks,
        ),
        Some("The average `Number of Employees` is `15` across `2` rows.".to_string())
    );
}

#[test]
fn build_table_summary_grounded_answer_derives_average_from_table_rows() {
    let document_id = Uuid::now_v7();
    let chunks = (1..=4)
        .map(|value| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.25),
            source_text: format!("Sheet: Sheet1 | Row {value} | col_1: {value}"),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "What is the average col_1 value in sample-heavy-1.xls?",
            Some(&table_average_ir()),
            &chunks
        ),
        Some("The average `col_1` is `2.50` across `4` rows.".to_string())
    );
}

#[test]
fn render_table_summary_chunk_section_derives_from_table_rows() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.25),
            source_text: "Sheet: Sheet1 | Row 1 | col_1: 1".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.25),
            source_text: "Sheet: Sheet1 | Row 2 | col_1: 3".to_string(),
        },
    ];

    let section = render_table_summary_chunk_section(
        "What is the average col_1 value in sample-heavy-1.xls?",
        Some(&table_average_ir()),
        &chunks,
    );
    assert!(section.contains("Table summaries"));
    assert!(section.contains("Average: 2"));
}

#[test]
fn build_missing_explicit_document_answer_reports_absent_file_reference() {
    let document = KnowledgeDocumentRow {
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: format!("fixture-{}", Uuid::now_v7()),
        file_name: Some("organizations-100.csv".to_string()),
        title: Some("organizations-100.csv".to_string()),
        source_uri: None,
        document_hint: None,
        document_state: "active".to_string(),
        active_revision_id: None,
        readable_revision_id: None,
        latest_revision_no: None,
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        deleted_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let index = HashMap::from([(document.document_id, document)]);

    assert_eq!(
        build_missing_explicit_document_answer(
            "What is sample record's job title in records-a.csv?",
            &index,
        ),
        Some("Document `records-a.csv` is not present in the active library.".to_string())
    );
}
