use super::*;
use uuid::Uuid;

fn document_row(
    external_key: &str,
    file_name: Option<&str>,
    document_state: &str,
    readable_revision_id: Option<Uuid>,
) -> KnowledgeDocumentRow {
    KnowledgeDocumentRow {
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: external_key.to_string(),
        file_name: file_name.map(str::to_string),
        title: file_name.map(str::to_string),
        source_uri: None,
        document_hint: None,
        document_state: document_state.to_string(),
        active_revision_id: None,
        readable_revision_id,
        latest_revision_no: None,
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        deleted_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn index_with(row: &KnowledgeDocumentRow) -> HashMap<Uuid, KnowledgeDocumentRow> {
    HashMap::from([(row.document_id, row.clone())])
}

const QUESTION: &str = "What is the job title in records-a.csv?";

#[test]
fn store_read_gate_is_false_without_an_explicit_document_literal() {
    assert!(!absent_explicit_document_literal_exists("How do I configure it?", &HashMap::new()));
}

#[test]
fn store_read_gate_is_false_when_every_literal_is_already_indexed() {
    let searchable = document_row("records-a.csv", Some("records-a.csv"), "active", None);

    assert!(!absent_explicit_document_literal_exists(QUESTION, &index_with(&searchable)));
}

#[test]
fn store_read_gate_is_true_for_a_literal_absent_from_the_index() {
    assert!(absent_explicit_document_literal_exists(QUESTION, &HashMap::new()));
}

#[test]
fn literal_present_in_the_searchable_index_needs_no_statement() {
    let searchable = document_row("records-a.csv", Some("records-a.csv"), "active", None);

    let answer = classify_document_presence_override(
        QUESTION,
        &index_with(&searchable),
        &[],
        QueryLanguage::En,
    );

    assert!(answer.is_none());
}

#[test]
fn searchable_store_row_leaves_the_grounded_answer_untouched() {
    let document =
        document_row("records-a.csv", Some("records-a.csv"), "active", Some(Uuid::now_v7()));

    let answer = classify_document_presence_override(
        QUESTION,
        &index_with(&document),
        std::slice::from_ref(&document),
        QueryLanguage::En,
    );

    assert!(answer.is_none());
}

#[test]
fn present_but_not_active_reports_it_cannot_be_searched_yet() {
    let documents =
        [document_row("records-a.csv", Some("records-a.csv"), "failed", Some(Uuid::now_v7()))];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::En,
    )
    .expect("non-active library row produces a statement");

    assert_eq!(
        answer.answer,
        "Document `records-a.csv` is present in the library but is not active and cannot be searched yet."
    );
    assert_eq!(answer.answer_kind, AnswerKind::MissingExplicitDocument);
    assert!(!answer.answer.contains("not present"));
    assert!(!answer.answer.contains("failed"), "no raw lifecycle state: {}", answer.answer);
}

#[test]
fn present_without_a_readable_revision_names_the_revision_gap() {
    let documents = [document_row("records-a.csv", Some("records-a.csv"), "active", None)];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::En,
    )
    .expect("unreadable library row produces a statement");

    assert_eq!(
        answer.answer,
        "Document `records-a.csv` is present in the library but has no readable revision, so it cannot be searched."
    );
}

#[test]
fn present_inactive_without_a_readable_revision_lists_both_reasons() {
    let documents = [document_row("records-a.csv", Some("records-a.csv"), "failed", None)];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::En,
    )
    .expect("inactive unreadable library row produces a statement");

    assert_eq!(
        answer.answer,
        "Document `records-a.csv` is present in the library but is not active and has no readable revision, so it cannot be searched."
    );
}

#[test]
fn active_row_with_a_readable_revision_absent_from_the_index_names_the_preparation_gap() {
    // Active with a readable revision, but the searchable index additionally
    // requires a `prepared` structured revision, so the row is genuinely
    // unsearchable while "has no readable revision" would be false.
    let documents =
        [document_row("records-a.csv", Some("records-a.csv"), "active", Some(Uuid::now_v7()))];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::En,
    )
    .expect("active row outside the searchable index produces a statement");

    assert_eq!(
        answer.answer,
        "Document `records-a.csv` is present in the library but its searchable content is not prepared yet."
    );
    assert!(!answer.answer.contains("not present"));
    assert!(!answer.answer.contains("no readable revision"), "{}", answer.answer);
}

#[test]
fn truly_absent_literal_keeps_the_absent_message() {
    let answer =
        classify_document_presence_override(QUESTION, &HashMap::new(), &[], QueryLanguage::En)
            .expect("absent statement");

    assert_eq!(answer.answer, "Document `records-a.csv` is not present in the active library.");
}

#[test]
fn a_near_miss_literal_is_not_reported_as_present() {
    let documents = [document_row("records-b.csv", Some("records-b.csv"), "failed", None)];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::En,
    )
    .expect("absent statement");

    assert_eq!(answer.answer, "Document `records-a.csv` is not present in the active library.");
}

#[test]
fn colon_joined_external_key_matches_on_its_segment() {
    let documents = [document_row("connector:records-a.csv", None, "failed", Some(Uuid::now_v7()))];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::En,
    )
    .expect("colon-joined key produces a statement");

    assert_eq!(
        answer.answer,
        "Document `records-a.csv` is present in the library but is not active and cannot be searched yet."
    );
}

#[test]
fn russian_question_language_selects_russian_copy() {
    let documents =
        [document_row("records-a.csv", Some("records-a.csv"), "failed", Some(Uuid::now_v7()))];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::Ru,
    )
    .expect("russian statement");

    assert!(answer.answer.contains("есть в библиотеке"), "{}", answer.answer);
    assert!(answer.answer.contains("records-a.csv"), "{}", answer.answer);
    assert!(!answer.answer.contains("failed"), "{}", answer.answer);
}

#[test]
fn russian_question_language_selects_russian_revision_gap_copy() {
    let documents = [document_row("records-a.csv", Some("records-a.csv"), "active", None)];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::Ru,
    )
    .expect("russian statement");

    assert_eq!(
        answer.answer,
        "Документ `records-a.csv` есть в библиотеке, но у него нет читаемой ревизии, поэтому поискать его нельзя."
    );
}

#[test]
fn russian_question_language_selects_russian_not_prepared_copy() {
    let documents =
        [document_row("records-a.csv", Some("records-a.csv"), "active", Some(Uuid::now_v7()))];

    let answer = classify_document_presence_override(
        QUESTION,
        &HashMap::new(),
        &documents,
        QueryLanguage::Ru,
    )
    .expect("russian statement");

    assert_eq!(
        answer.answer,
        "Документ `records-a.csv` есть в библиотеке, но его содержимое ещё не подготовлено к поиску."
    );
}
