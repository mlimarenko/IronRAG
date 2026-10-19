use super::*;

use uuid::Uuid;

fn identity_row(document_id: Uuid, row_number: usize, fields: &[(&str, &str)]) -> ParsedTableRow {
    ParsedTableRow {
        document_id,
        sheet_name: "sheet_01".to_string(),
        table_name: Some("status_table".to_string()),
        row_number,
        fields: fields
            .iter()
            .map(|(header, value)| ((*header).to_string(), (*value).to_string()))
            .collect(),
        flattened_text: fields
            .iter()
            .flat_map(|(header, value)| [*header, *value])
            .collect::<Vec<_>>()
            .join(" "),
        score: 1.0,
    }
}

#[test]
fn plural_header_does_not_stem_to_singular() {
    let question = "What HTTP methods should be used for CRUD operations?";
    let lowered = question.to_lowercase();
    let question_tokens = normalized_alnum_tokens(question, 2);

    assert!(!table_header_matches_question("**Method**", &lowered, &question_tokens));
    assert!(table_header_matches_question("**CRUD**", &lowered, &question_tokens));
}

#[test]
fn identity_tokens_shared_by_every_table_row_are_not_distinctive() {
    let document_id = Uuid::now_v7();
    let rows = vec![
        identity_row(
            document_id,
            1,
            &[("Code", "403"), ("Description", "shared marker"), ("Context", "shared context")],
        ),
        identity_row(
            document_id,
            2,
            &[("Code", "404"), ("Description", "shared marker"), ("Context", "other context")],
        ),
    ];

    assert!(!row_identifier_match_is_distinctive_within_table(
        "What Code is specified for the shared context?",
        &rows[0],
        &rows,
    ));
}

#[test]
fn identity_token_unique_to_one_table_row_is_distinctive() {
    let document_id = Uuid::now_v7();
    let rows = vec![
        identity_row(
            document_id,
            1,
            &[("Code", "403"), ("Description", "insufficient permissions")],
        ),
        identity_row(document_id, 2, &[("Code", "404"), ("Description", "missing resource")]),
    ];

    assert!(row_identifier_match_is_distinctive_within_table(
        "What Description is listed for 403?",
        &rows[0],
        &rows,
    ));
}
