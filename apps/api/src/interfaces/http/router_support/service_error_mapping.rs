use anyhow::Error as AnyhowError;
use sqlx::Error as SqlxError;
use tracing::{Span, error};
use uuid::Uuid;

use super::api_error::ApiError;
use crate::services::{
    content::error::ContentServiceError, graph::error::GraphServiceError,
    ingest::error::IngestServiceError, knowledge::error::KnowledgeServiceError,
    query::error::QueryServiceError, webhook::error::WebhookServiceError,
};

pub(crate) fn record_error_kind(error_kind: &'static str) {
    Span::current().record("error.kind", error_kind);
}

/// Logs a redacted webhook service failure. The macro lives in this leaf helper
/// because a tracing macro's expansion is itself counted as cognitive
/// complexity; the event level, structured field and message are unchanged.
fn log_redacted_service_failure(error_kind: &'static str, message: &'static str) {
    tracing::error!(error_kind = error_kind, "{}", message);
}

impl From<ContentServiceError> for ApiError {
    fn from(error: ContentServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            ContentServiceError::NotFound { message } => Self::NotFound(message),
            ContentServiceError::InvalidRequest { message } => Self::BadRequest(message),
            ContentServiceError::StateConflict { message } => Self::Conflict(message),
            ContentServiceError::StorageUnavailable { message } => {
                Self::ServiceUnavailable { message, kind: "content_storage_unavailable" }
            }
            ContentServiceError::ProviderUnavailable { message } => Self::ProviderFailure(message),
            ContentServiceError::Cancelled => {
                Self::Conflict("content operation cancelled".to_string())
            }
            ContentServiceError::Repository(error) => {
                Self::internal_with_log(error, "content service repository failure")
            }
            ContentServiceError::Internal(error) => {
                Self::internal_with_log(error, "content service internal failure")
            }
        }
    }
}

impl From<GraphServiceError> for ApiError {
    fn from(error: GraphServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            GraphServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            GraphServiceError::NotFound { message } => Self::NotFound(message),
            GraphServiceError::StateConflict { message } => Self::Conflict(message),
            GraphServiceError::RevisionSuperseded { revision_id } => Self::StaleRevision(format!(
                "graph revision {revision_id} is no longer the active readable head"
            )),
            GraphServiceError::WriteContention { message } => Self::GraphWriteContention(message),
            GraphServiceError::PersistenceIntegrity { message } => {
                Self::GraphPersistenceIntegrity(message)
            }
            GraphServiceError::ProviderUnavailable { message } => Self::ProviderFailure(message),
            GraphServiceError::Cancelled => Self::Conflict("graph operation cancelled".to_string()),
            GraphServiceError::Repository(error) => {
                Self::internal_with_log(error, "graph service repository failure")
            }
            GraphServiceError::Internal(error) => {
                Self::internal_with_log(error, "graph service internal failure")
            }
        }
    }
}

impl From<IngestServiceError> for ApiError {
    fn from(error: IngestServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            IngestServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            IngestServiceError::BindingNotConfigured { message }
            | IngestServiceError::StateConflict { message } => Self::Conflict(message),
            IngestServiceError::Provider(failure) => {
                Self::ProviderFailure(failure.message().to_string())
            }
            IngestServiceError::Cancelled => {
                Self::Conflict("ingest operation cancelled".to_string())
            }
            IngestServiceError::Repository(error) => {
                Self::internal_with_log(error, "ingest service repository failure")
            }
            IngestServiceError::Internal(error) => {
                Self::internal_with_log(error, "ingest service internal failure")
            }
        }
    }
}

impl From<KnowledgeServiceError> for ApiError {
    fn from(error: KnowledgeServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            KnowledgeServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            KnowledgeServiceError::NotFound { message } => Self::NotFound(message),
            KnowledgeServiceError::GraphNotReady { message } => Self::KnowledgeNotReady(message),
            KnowledgeServiceError::GraphIntegrity { message } => Self::KnowledgeNotReady(message),
            KnowledgeServiceError::CacheUnavailable { message } => {
                Self::ServiceUnavailable { message, kind: "cache_unavailable" }
            }
            KnowledgeServiceError::Repository(error) => {
                Self::internal_with_log(error, "knowledge service repository failure")
            }
            KnowledgeServiceError::Internal(error) => {
                Self::internal_with_log(error, "knowledge service internal failure")
            }
        }
    }
}

impl From<QueryServiceError> for ApiError {
    fn from(error: QueryServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            QueryServiceError::LibraryNotFound { library_id } => {
                Self::resource_not_found("library", library_id)
            }
            QueryServiceError::NotFound { message } => Self::NotFound(message),
            QueryServiceError::BindingNotConfigured { message }
            | QueryServiceError::StateConflict { message } => Self::Conflict(message),
            QueryServiceError::Provider(failure) => {
                Self::ProviderFailure(failure.message().to_string())
            }
            QueryServiceError::CacheUnavailable { message } => {
                Self::ServiceUnavailable { message, kind: "cache_unavailable" }
            }
            QueryServiceError::Cancelled => Self::Conflict("query operation cancelled".to_string()),
            QueryServiceError::DeadlineExceeded => Self::query_deadline_exceeded(),
            QueryServiceError::Repository(error) => {
                Self::internal_with_log(error, "query service repository failure")
            }
            QueryServiceError::Internal(error) => {
                Self::internal_with_log(error, "query service internal failure")
            }
        }
    }
}

impl From<WebhookServiceError> for ApiError {
    fn from(error: WebhookServiceError) -> Self {
        record_error_kind(error.kind());
        match error {
            WebhookServiceError::DeliveryAttemptNotFound { job_id } => {
                Self::resource_not_found("webhook_delivery_attempt_for_job", job_id)
            }
            WebhookServiceError::SubscriptionNotFound { subscription_id } => {
                Self::resource_not_found("webhook_subscription", subscription_id)
            }
            WebhookServiceError::DeliveryLeaseInFlight { retry_at, .. } => Self::Conflict(format!(
                "webhook delivery lease is still in flight until {retry_at}"
            )),
            WebhookServiceError::DeliveryCanceled { .. } => {
                Self::Conflict("webhook delivery operation cancelled".to_string())
            }
            WebhookServiceError::StateConflict { message } => Self::Conflict(message),
            WebhookServiceError::Repository(_) => {
                log_redacted_service_failure(
                    "WebhookServiceError::Repository",
                    "webhook service repository failure (detail redacted)",
                );
                Self::Internal
            }
            WebhookServiceError::CredentialProtection(error) => Self::from_secret_encryption(error),
            WebhookServiceError::Internal(_) => {
                log_redacted_service_failure(
                    "WebhookServiceError::Internal",
                    "webhook service internal failure (detail redacted)",
                );
                Self::Internal
            }
        }
    }
}

pub fn map_runtime_lifecycle_error(error: AnyhowError) -> ApiError {
    let error = match error.downcast::<ApiError>() {
        Ok(error) => return error,
        Err(error) => error,
    };
    let error = match error.downcast::<ContentServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<GraphServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<IngestServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<KnowledgeServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<QueryServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    let error = match error.downcast::<WebhookServiceError>() {
        Ok(error) => return error.into(),
        Err(error) => error,
    };
    error!(error = ?error, "runtime lifecycle handler failed with unexpected internal error");
    ApiError::Internal
}

#[must_use]
pub fn map_workspace_create_error(error: SqlxError, slug: &str) -> ApiError {
    match crate::shared::database::classify_error(&error) {
        crate::shared::database::DatabaseErrorClass::UniqueViolation => {
            ApiError::Conflict(format!("workspace slug '{slug}' already exists"))
        }
        crate::shared::database::DatabaseErrorClass::Contention => ApiError::database_contention(),
        _ => ApiError::Internal,
    }
}

#[must_use]
pub fn map_library_create_error(error: SqlxError, workspace_id: Uuid, slug: &str) -> ApiError {
    match crate::shared::database::classify_error(&error) {
        crate::shared::database::DatabaseErrorClass::UniqueViolation => {
            ApiError::Conflict(format!("library slug '{slug}' already exists in this workspace"))
        }
        crate::shared::database::DatabaseErrorClass::ForeignKeyViolation => {
            ApiError::NotFound(format!("workspace {workspace_id} not found"))
        }
        crate::shared::database::DatabaseErrorClass::Contention => ApiError::database_contention(),
        _ => ApiError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        ApiError, map_library_create_error, map_runtime_lifecycle_error, map_workspace_create_error,
    };
    use crate::{
        services::{graph::error::GraphServiceError, webhook::error::WebhookServiceError},
        shared::database::test_database_error,
    };

    #[test]
    fn maps_typed_graph_write_contention_to_specific_kind() {
        let error =
            map_runtime_lifecycle_error(anyhow::Error::new(GraphServiceError::WriteContention {
                message: "opaque".to_string(),
            }));
        assert!(matches!(error, ApiError::GraphWriteContention(_)));
    }

    #[test]
    fn unknown_runtime_lifecycle_message_is_always_internal() {
        let error = map_runtime_lifecycle_error(anyhow::anyhow!(
            "stale revision conflict provider failure upstream timeout missing price deadlock"
        ));

        assert!(matches!(error, ApiError::Internal));
    }

    #[test]
    fn maps_webhook_delivery_lease_contention_to_conflict_without_internal_ids() {
        let retry_at = Utc::now();
        let attempt_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let error = ApiError::from(WebhookServiceError::DeliveryLeaseInFlight {
            attempt_id,
            job_id,
            retry_at,
        });

        assert_eq!(error.kind(), "conflict");
        assert_eq!(error.status_code(), StatusCode::CONFLICT);
        assert!(error.to_string().contains(&retry_at.to_string()));
        assert!(!error.to_string().contains(&attempt_id.to_string()));
        assert!(!error.to_string().contains(&job_id.to_string()));
    }

    #[test]
    fn maps_webhook_delivery_cancellation_to_conflict_without_internal_ids() {
        let attempt_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let error = ApiError::from(WebhookServiceError::DeliveryCanceled { attempt_id, job_id });

        assert_eq!(error.kind(), "conflict");
        assert_eq!(error.status_code(), StatusCode::CONFLICT);
        assert_eq!(error.to_string(), "conflict: webhook delivery operation cancelled");
        assert!(!error.to_string().contains(&attempt_id.to_string()));
        assert!(!error.to_string().contains(&job_id.to_string()));
    }

    #[test]
    fn maps_workspace_unique_violations_to_conflict() {
        let error = map_workspace_create_error(
            test_database_error("23505", Some("workspace_slug_key")),
            "agent-workspace",
        );

        assert!(matches!(error, ApiError::Conflict(_)));
        assert_eq!(error.to_string(), "conflict: workspace slug 'agent-workspace' already exists");
    }

    #[test]
    fn workspace_database_contention_is_retryable_and_other_failures_are_internal() {
        let contention = map_workspace_create_error(
            crate::shared::database::test_database_error("40001", None),
            "agent-workspace",
        );
        assert!(matches!(contention, ApiError::ServiceUnavailable { .. }));
        assert_eq!(contention.kind(), "database_contention");
        assert!(matches!(
            map_workspace_create_error(
                crate::shared::database::test_database_error("22000", None),
                "agent-workspace"
            ),
            ApiError::Internal
        ));
    }

    #[test]
    fn maps_library_unique_violations_to_conflict() {
        let error = map_library_create_error(
            test_database_error("23505", Some("project_workspace_id_slug_key")),
            Uuid::nil(),
            "agent-library",
        );

        assert!(matches!(error, ApiError::Conflict(_)));
        assert_eq!(
            error.to_string(),
            "conflict: library slug 'agent-library' already exists in this workspace"
        );
    }

    #[test]
    fn library_database_contention_is_retryable_and_other_failures_are_internal() {
        let contention = map_library_create_error(
            crate::shared::database::test_database_error("40P01", None),
            Uuid::nil(),
            "agent-library",
        );
        assert!(matches!(contention, ApiError::ServiceUnavailable { .. }));
        assert_eq!(contention.kind(), "database_contention");
        assert!(matches!(
            map_library_create_error(
                crate::shared::database::test_database_error("22000", None),
                Uuid::nil(),
                "agent-library"
            ),
            ApiError::Internal
        ));
    }

    #[test]
    fn maps_library_foreign_key_violations_to_not_found() {
        let workspace_id = Uuid::now_v7();
        let error = map_library_create_error(
            test_database_error("23503", Some("project_workspace_id_fkey")),
            workspace_id,
            "agent-library",
        );

        assert!(matches!(error, ApiError::NotFound(_)));
        assert_eq!(error.to_string(), format!("not found: workspace {workspace_id} not found"));
    }
}
