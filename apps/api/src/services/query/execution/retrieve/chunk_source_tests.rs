use super::test_prelude::*;

#[test]
fn chunk_answer_source_text_preserves_content_outside_window() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut row = chunk_row(
        workspace_id,
        library_id,
        document_id,
        revision_id,
        0,
        "Overview line.\n1. Theta marker changed from state K to state L.",
    );
    row.chunk_kind = Some("paragraph".to_string());
    row.window_text = Some("Overview line.".to_string());

    let source_text = chunk_answer_source_text(&row);

    assert!(source_text.contains("Overview line."), "{source_text}");
    assert!(source_text.contains("Theta marker changed from state K to state L"), "{source_text}");
}
