use super::*;

use crate::services::query::execution::preflight::document_presence::classify_document_presence_override;

#[test]
fn absent_file_reference_reports_the_literal() {
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

    let answer = classify_document_presence_override(
        "What is sample record's job title in records-a.csv?",
        &index,
        &[],
        QueryLanguage::En,
    )
    .expect("absent explicit reference produces a deterministic statement");

    assert_eq!(answer.answer, "Document `records-a.csv` is not present in the active library.");
}
