use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::super::super::source_focus_selection::{
    SourceFocusLimits, select_query_focused_source_rows_with_declaration_reach,
};
use super::super::{map_companion_chunk, select_query_focused_source_rows};
use super::{chunk_row, document_row};

const TAIL_CHUNK_INDEX: i32 = 72;
const HEAD_CHUNK_INDEX: i32 = 3;

#[test]
fn document_scoped_query_reaches_a_matching_declaration_block_in_a_long_document() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(
            document_id,
            revision_id,
            HEAD_CHUNK_INDEX,
            "paragraph",
            "  worker:\n    bindings:\n      - private\n    note: restricted overview",
        ),
        chunk_row(
            document_id,
            revision_id,
            TAIL_CHUNK_INDEX,
            "paragraph",
            "bindings:\n  public:\n    driver: shared\n  private:\n    driver: isolated\n    restricted: true",
        ),
    ];
    let selected = select_query_focused_source_rows(
        &rows,
        &["bindings".to_string(), "restricted".to_string()],
        false,
        &[],
        1,
    );

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, TAIL_CHUNK_INDEX);

    let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
    let mapped = map_companion_chunk(
        selected.into_iter().next().expect("selected declaration block"),
        1.0,
        &document_index,
        &["bindings".to_string(), "restricted".to_string()],
    )
    .expect("mapped declaration block");

    assert!(mapped.excerpt.starts_with("bindings:\n  public:"));
    assert!(mapped.excerpt.contains("  private:\n    driver: isolated\n    restricted: true"));
}

#[test]
fn query_focused_source_rows_do_not_widen_to_unrelated_declarations() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(
            document_id,
            revision_id,
            HEAD_CHUNK_INDEX,
            "paragraph",
            "retention policy for archived records",
        ),
        chunk_row(
            document_id,
            revision_id,
            TAIL_CHUNK_INDEX,
            "paragraph",
            "bindings:\n  public:\n    driver: shared\n  private:\n    restricted: true",
        ),
    ];
    let selected =
        select_query_focused_source_rows(&rows, &["retention".to_string()], false, &[], 1);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, HEAD_CHUNK_INDEX);
}

#[test]
fn late_matching_declaration_wins_a_slot_within_the_caller_limit() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows = vec![
        chunk_row(
            document_id,
            revision_id,
            HEAD_CHUNK_INDEX,
            "paragraph",
            "LedgerBoard records state summary overview",
        ),
        chunk_row(
            document_id,
            revision_id,
            TAIL_CHUNK_INDEX,
            "paragraph",
            "function LedgerBoard() {\n  const state = loadEntries();\n}",
        ),
    ];
    let focus = [
        "ledgerboard".to_string(),
        "records".to_string(),
        "state".to_string(),
        "summary".to_string(),
    ];

    let selected = select_query_focused_source_rows(&rows, &focus, false, &[], 1);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].chunk_index, TAIL_CHUNK_INDEX);

    let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
    let mapped = map_companion_chunk(selected[0].clone(), 1.0, &document_index, &focus)
        .expect("mapped declaration row");
    assert!(mapped.excerpt.contains("function LedgerBoard()"));
    assert!(mapped.excerpt.contains("const state = loadEntries();"));
}

#[test]
fn declaration_free_prose_keeps_exactly_the_existing_evidence_selection() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let rows_without_declaration = vec![
        chunk_row(
            document_id,
            revision_id,
            HEAD_CHUNK_INDEX,
            "paragraph",
            "Ledger records retain their state for the reporting period.",
        ),
        chunk_row(
            document_id,
            revision_id,
            TAIL_CHUNK_INDEX,
            "paragraph",
            "The reporting appendix describes ledger state in prose.",
        ),
    ];
    let focus = ["ledger".to_string(), "state".to_string(), "reporting".to_string()];
    let mut rows_with_declaration = rows_without_declaration.clone();
    rows_with_declaration[1].content_text =
        "function ledger() {\n  const state = reportingPeriod();\n}".to_string();

    let without_declaration = select_query_focused_source_rows_with_declaration_reach(
        &rows_without_declaration,
        &focus,
        false,
        &[],
        SourceFocusLimits { limit: 1, additional_limit: 1 },
        &HashSet::new(),
    );
    let with_declaration = select_query_focused_source_rows_with_declaration_reach(
        &rows_with_declaration,
        &focus,
        false,
        &[],
        SourceFocusLimits { limit: 1, additional_limit: 1 },
        &HashSet::new(),
    );

    assert_eq!(without_declaration.len(), 1);
    assert_eq!(without_declaration[0].row.chunk_index, HEAD_CHUNK_INDEX);
    assert_eq!(with_declaration.len(), 1);
    assert_eq!(with_declaration[0].row.chunk_index, TAIL_CHUNK_INDEX);
}
