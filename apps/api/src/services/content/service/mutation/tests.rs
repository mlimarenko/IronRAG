use super::admission::{
    delete_mutation_item_is_reusable, map_admission_database_error, map_atomic_admission_error,
};
use super::*;

use crate::shared::database::test_database_error;

#[test]
fn unique_database_failures_map_to_external_key_conflict() {
    assert!(matches!(
        map_admission_database_error(test_database_error("23505", None)),
        ApiError::Conflict(message)
            if message == "document external key already exists"
    ));
}

#[test]
fn admission_contention_is_retryable_and_unknown_failures_are_internal() {
    let contention = map_admission_database_error(test_database_error("40001", None));
    assert_eq!(contention.kind(), "database_contention");
    assert!(matches!(contention, ApiError::ServiceUnavailable { .. }));
    assert!(matches!(
        map_admission_database_error(test_database_error("22000", None)),
        ApiError::Internal
    ));
}

#[test]
fn target_state_errors_map_without_reclassifying_unrelated_database_failures() {
    let document_id = Uuid::now_v7();

    assert!(matches!(
        map_atomic_admission_error(AdmissionError::TargetDocumentNotFound { document_id }),
        ApiError::NotFound(_)
    ));
    assert!(matches!(
        map_atomic_admission_error(AdmissionError::TargetDocumentDeleted { document_id }),
        ApiError::BadRequest(_)
    ));
    assert!(matches!(
        map_atomic_admission_error(AdmissionError::TargetDocumentScopeConflict { document_id }),
        ApiError::Conflict(_)
    ));
    assert!(matches!(
        map_atomic_admission_error(AdmissionError::TargetDocumentHeadIntegrity { document_id }),
        ApiError::Internal
    ));
    assert!(matches!(
        map_atomic_admission_error(AdmissionError::ConflictingActiveMutation { document_id }),
        ApiError::ConflictingMutation(_)
    ));
    assert!(matches!(
        map_atomic_admission_error(AdmissionError::Database(sqlx::Error::RowNotFound)),
        ApiError::Internal
    ));
}

#[test]
fn delete_item_reuse_requires_the_exact_document_and_revision_scope() {
    let document_id = Uuid::now_v7();
    let base_revision_id = Uuid::now_v7();
    let mut item = ContentMutationItem {
        id: Uuid::now_v7(),
        mutation_id: Uuid::now_v7(),
        document_id: None,
        base_revision_id: Some(base_revision_id),
        result_revision_id: None,
        item_state: "pending".to_string(),
        message: Some("document delete admitted".to_string()),
    };

    assert!(!delete_mutation_item_is_reusable(
        &item,
        document_id,
        Some(base_revision_id),
        "pending",
        "document delete admitted",
    ));
    item.item_state = "applied".to_string();
    assert!(!delete_mutation_item_is_reusable(
        &item,
        document_id,
        Some(base_revision_id),
        "pending",
        "document delete admitted",
    ));
    item.document_id = Some(document_id);
    assert!(delete_mutation_item_is_reusable(
        &item,
        document_id,
        Some(base_revision_id),
        "pending",
        "document delete admitted",
    ));
}
