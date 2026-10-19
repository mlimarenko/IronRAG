use super::*;

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
