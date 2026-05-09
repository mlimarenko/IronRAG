use uuid::Uuid;

use crate::{infra::repositories::content_repository, interfaces::http::router_support::ApiError};

pub(super) fn ensure_existing_mutation_matches_request(
    existing: &content_repository::ContentMutationRow,
    request_workspace_id: Uuid,
    request_library_id: Uuid,
    request_operation_kind: &str,
    request_source_identity: Option<&str>,
) -> Result<(), ApiError> {
    if existing.workspace_id != request_workspace_id
        || existing.library_id != request_library_id
        || existing.operation_kind != request_operation_kind
    {
        return Err(ApiError::idempotency_conflict(
            "the same idempotency key was already used for a different mutation request",
        ));
    }
    if let Some(request_source_identity) = request_source_identity {
        match existing.source_identity.as_deref() {
            Some(existing_source_identity)
                if existing_source_identity != request_source_identity =>
            {
                return Err(ApiError::idempotency_conflict(
                    "the same idempotency key was already used with a different payload",
                ));
            }
            None => {
                return Err(ApiError::idempotency_conflict(
                    "the same idempotency key was already used before payload identity tracking was available; retry with a new idempotency key",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn is_content_mutation_idempotency_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            database_error.constraint() == Some("idx_content_mutation_idempotency")
        }
        _ => false,
    }
}
