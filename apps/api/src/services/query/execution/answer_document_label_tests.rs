use std::collections::HashMap;

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR, QueryLanguage, QueryScope};
use crate::infra::arangodb::document_store::KnowledgeDocumentRow;
use crate::services::query::execution::types::RuntimeMatchedChunk;
use crate::shared::extraction::table_summary::{
    build_table_column_summaries, render_table_column_summary,
};

use super::super::{
    build_missing_explicit_document_answer, build_table_row_grounded_answer,
    build_table_summary_grounded_answer, concise_document_subject_label,
    document_focus_marker_hits, focused_answer_document_id, render_table_summary_chunk_section,
};

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
        confidence: 0.0,
    }
}

fn table_row_inventory_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        target_types: vec!["table_row".to_string()],
        ..describe_query_ir()
    }
}

fn table_average_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec!["table_average".to_string()],
        ..describe_query_ir()
    }
}

fn table_frequency_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec!["table_frequency".to_string()],
        ..describe_query_ir()
    }
}

#[test]
fn concise_document_subject_label_strips_spreadsheet_extensions() {
    assert_eq!(
        concise_document_subject_label("spreadsheet_ods_api_reference.xlsb"),
        "Spreadsheet ODS API reference"
    );
    assert_eq!(concise_document_subject_label("inventory_snapshot.ods"), "Inventory snapshot");
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
            document_label: "people-100.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "Sheet: people-100 | Row 1 | Email: elijah57@example.net".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: xlsx_id,
            document_label: "people-100.xlsx".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: "Sheet: people-100 | Row 1 | Email: elijah57@example.net".to_string(),
        },
    ];

    assert_eq!(
        focused_answer_document_id(
            "In people-100.csv what is Shelby Terrell's job title (email elijah57@example.net)?",
            &chunks,
        ),
        Some(csv_id)
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

    let ir = describe_query_ir();

    assert_eq!(
        build_table_row_grounded_answer("Show the first 5 rows from sample-heavy-1.xls.", Some(&ir), &chunks),
        Some(
            "- Row 1: col_1 = `1`\n- Row 2: col_1 = `2`\n- Row 3: col_1 = `3`\n- Row 4: col_1 = `4`\n- Row 5: col_1 = `5`"
                .to_string()
        )
    );
}

#[test]
fn build_table_row_grounded_answer_supports_russian_industry_synonym() {
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
        Some("Country: `Papua New Guinea`; Industry: `Plastics`".to_string())
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
fn build_table_row_grounded_answer_lists_values_for_russian_listed_marker() {
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
        Some("customers-100"),
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
            document_label: "customers-100.csv".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "What is the most frequent City in customers-100.csv?",
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
        key: "organizations-100.csv".to_string(),
        arango_id: None,
        arango_rev: None,
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: "organizations-100.csv".to_string(),
        file_name: Some("organizations-100.csv".to_string()),
        title: Some("organizations-100.csv".to_string()),
        document_state: "active".to_string(),
        active_revision_id: None,
        readable_revision_id: None,
        latest_revision_no: None,
        deleted_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let index = HashMap::from([(document.document_id, document)]);

    assert_eq!(
        build_missing_explicit_document_answer(
            "What is Shelby Terrell's job title in people-100.csv?",
            &index,
        ),
        Some("Document `people-100.csv` is not present in the active library.".to_string())
    );
}
