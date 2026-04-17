use std::collections::HashMap;

use uuid::Uuid;

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
            document_id: csv_id,
            document_label: "people-100.csv".to_string(),
            excerpt: String::new(),
            score: Some(1.0),
            source_text: "Sheet: people-100 | Row 1 | Email: elijah57@example.net".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: xlsx_id,
            document_label: "people-100.xlsx".to_string(),
            excerpt: String::new(),
            score: Some(10.0),
            source_text: "Sheet: people-100 | Row 1 | Email: elijah57@example.net".to_string(),
        },
    ];

    assert_eq!(
        focused_answer_document_id(
            "В people-100.csv какая должность у Shelby Terrell с email elijah57@example.net?",
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
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score: Some(10.0 - row_number as f32),
            source_text: format!("Sheet: test1 | Row {row_number} | col_1: {row_number}"),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_row_grounded_answer("Покажи значения из первых 5 строк sample-heavy-1.xls.", None, &chunks),
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
        document_id,
        document_label: "organizations-100.csv".to_string(),
        excerpt: String::new(),
        score: Some(10.0),
        source_text:
            "Sheet: organizations-100 | Row 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics"
                .to_string(),
    }];

    assert_eq!(
        build_table_row_grounded_answer(
            "В organizations-100.csv какая страна и индустрия у Ferrell LLC?",
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
            document_id,
            document_label: "sample-simple-2.xls".to_string(),
            excerpt: String::new(),
            score: Some(10.0),
            source_text: "Sheet: test1 | Row 1 | col_1: test1".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "sample-simple-2.xls".to_string(),
            excerpt: String::new(),
            score: Some(9.0),
            source_text: "Sheet: test2 | Row 1 | col_1: test2".to_string(),
        },
    ];

    assert_eq!(
        build_table_row_grounded_answer(
            "Какие значения есть в sample-simple-2.xls?",
            None,
            &chunks
        ),
        Some("- test1 row 1: `test1`\n- test2 row 1: `test2`".to_string())
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
            document_id,
            document_label: "organizations-100.csv".to_string(),
            excerpt: String::new(),
            score: Some(10.0 - index as f32),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "What is the most frequent Country in organizations-100.csv?",
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
            document_id,
            document_label: "customers-100.csv".to_string(),
            excerpt: String::new(),
            score: Some(10.0),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "В customers-100.csv какой город встречается чаще всего?",
            &chunks,
        ),
        Some(
            "Для `City` нет одного самого частого значения: все значения встречаются по одному разу."
                .to_string()
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
            document_id,
            document_label: "products-100.csv".to_string(),
            excerpt: String::new(),
            score: Some(10.0),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer("Какой средний stock в products-100.csv?", &chunks),
        Some("Среднее значение `Stock` — `200` по `3` строкам.".to_string())
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
            document_id,
            document_label: "organizations-100.csv".to_string(),
            excerpt: String::new(),
            score: Some(10.0),
            source_text: render_table_column_summary(&summary),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "Какое среднее число сотрудников в organizations-100.csv?",
            &chunks,
        ),
        Some("Среднее значение `Number of Employees` — `15` по `2` строкам.".to_string())
    );
}

#[test]
fn build_table_summary_grounded_answer_derives_average_from_table_rows() {
    let document_id = Uuid::now_v7();
    let chunks = (1..=4)
        .map(|value| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score: Some(0.25),
            source_text: format!("Sheet: Sheet1 | Row {value} | col_1: {value}"),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        build_table_summary_grounded_answer(
            "В sample-heavy-1.xls какое среднее значение?",
            &chunks
        ),
        Some("Среднее значение `col_1` — `2.50` по `4` строкам.".to_string())
    );
}

#[test]
fn render_table_summary_chunk_section_derives_from_table_rows() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score: Some(0.25),
            source_text: "Sheet: Sheet1 | Row 1 | col_1: 1".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "sample-heavy-1.xls".to_string(),
            excerpt: String::new(),
            score: Some(0.25),
            source_text: "Sheet: Sheet1 | Row 2 | col_1: 3".to_string(),
        },
    ];

    let section =
        render_table_summary_chunk_section("В sample-heavy-1.xls какое среднее значение?", &chunks);
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
            "У Shelby Terrell в people-100.csv какой job title?",
            &index,
        ),
        Some("Документ `people-100.csv` отсутствует в активной библиотеке.".to_string())
    );
}
