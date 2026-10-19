use super::*;
use sha2::{Digest as _, Sha256};

pub(super) fn map_atomic_admission_error(error: AdmissionError) -> ApiError {
    match error {
        AdmissionError::InvalidReplacementIdentity(error) => {
            ApiError::BadRequest(error.to_string())
        }
        AdmissionError::IdempotencyConflict { .. } => ApiError::IdempotencyConflict(
            "idempotency key was already used for a different content request".to_string(),
        ),
        AdmissionError::VisibilityIntentRequired { document_id } => {
            ApiError::VisibilityIntentRequired(format!(
                "document {document_id} requires a visibility intent"
            ))
        }
        AdmissionError::VisibilityIntentSuperseded { document_id } => {
            ApiError::VisibilityIntentSuperseded(format!(
                "visibility intent is not current for document {document_id}"
            ))
        }
        AdmissionError::VisibilityBindingConflict { document_id } => {
            ApiError::VisibilityBindingConflict(format!(
                "replacement does not match visibility binding for document {document_id}"
            ))
        }
        AdmissionError::TargetDocumentNotFound { document_id } => {
            ApiError::resource_not_found("document", document_id)
        }
        AdmissionError::TargetDocumentDeleted { .. } => {
            ApiError::BadRequest("deleted documents do not accept new mutations".to_string())
        }
        AdmissionError::TargetDocumentScopeConflict { .. } => {
            ApiError::Conflict("document scope changed before mutation admission".to_string())
        }
        AdmissionError::ConflictingActiveMutation { .. } => ApiError::ConflictingMutation(
            "document is still processing a previous mutation".to_string(),
        ),
        AdmissionError::DuplicateExternalKey { existing_document_id } => {
            ApiError::DuplicateContent {
                message: "an active document with this external key already exists".to_string(),
                existing_document_id,
            }
        }
        AdmissionError::DuplicateChecksum { existing_document_id } => ApiError::duplicate_content(
            "identical file already exists in this library",
            existing_document_id,
        ),
        AdmissionError::DocumentAdmissionIncomplete { document_id } => {
            ApiError::DocumentAdmissionIncomplete(format!(
                "document {document_id} has an incomplete admission that cannot be completed safely"
            ))
        }
        AdmissionError::Database(error) => map_admission_database_error(error),
        error @ AdmissionError::TargetDocumentHeadIntegrity { .. } => {
            ApiError::internal_with_log(error, "content admission head integrity failure")
        }
        other => ApiError::internal_with_log(other, "content admission failed"),
    }
}

pub(super) fn map_admission_database_error(error: sqlx::Error) -> ApiError {
    match classify_error(&error) {
        DatabaseErrorClass::UniqueViolation => {
            ApiError::Conflict("document external key already exists".to_string())
        }
        DatabaseErrorClass::ForeignKeyViolation => ApiError::Conflict(
            "document mutation references a resource that no longer exists".to_string(),
        ),
        DatabaseErrorClass::Contention => ApiError::database_contention(),
        DatabaseErrorClass::Other => ApiError::internal_with_log(error, "content admission failed"),
    }
}

pub(super) fn delete_mutation_item_is_reusable(
    item: &ContentMutationItem,
    document_id: Uuid,
    base_revision_id: Option<Uuid>,
    requested_state: &str,
    requested_message: &str,
) -> bool {
    let owns_target = item.document_id == Some(document_id)
        && item.base_revision_id == base_revision_id
        && item.result_revision_id.is_none();
    owns_target
        && ((item.item_state == "applied" && requested_state == "pending")
            || (item.item_state == requested_state
                && item.message.as_deref() == Some(requested_message)))
}

pub(super) fn validate_delete_mutation_item_collection(
    items: &[ContentMutationItem],
    document_id: Uuid,
    foreign_document_message: &str,
) -> Result<(), ApiError> {
    if items.iter().any(|item| item.document_id != Some(document_id)) {
        return Err(ApiError::idempotency_conflict(foreign_document_message));
    }
    Ok(())
}

pub(super) fn delete_mutation_request_identity(
    document_id: Uuid,
    source_identity: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ironrag:content-delete:v1\0");
    digest.update(document_id.as_bytes());
    match source_identity {
        Some(source_identity) => {
            digest.update([1]);
            digest.update(source_identity.as_bytes());
        }
        None => digest.update([0]),
    }
    format!("v1:sha256:{}", hex::encode(digest.finalize()))
}
