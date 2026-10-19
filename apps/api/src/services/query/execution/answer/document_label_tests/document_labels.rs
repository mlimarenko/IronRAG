use super::*;

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
