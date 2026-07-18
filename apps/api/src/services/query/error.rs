use thiserror::Error;
use uuid::Uuid;

use crate::{
    integrations::retry::ProviderCallError,
    interfaces::http::router_support::ApiError,
    services::{
        ingest::{cancellation::StageError, error::IngestServiceError},
        knowledge::error::KnowledgeServiceError,
    },
};

#[derive(Debug, Error)]
pub enum QueryServiceError {
    #[error("library {library_id} not found")]
    LibraryNotFound { library_id: Uuid },
    #[error("query resource not found: {message}")]
    NotFound { message: String },
    #[error("query binding not configured: {message}")]
    BindingNotConfigured { message: String },
    #[error("query state conflict: {message}")]
    StateConflict { message: String },
    #[error("query provider unavailable: {message}")]
    ProviderUnavailable { message: String },
    #[error("query cache unavailable: {message}")]
    CacheUnavailable { message: String },
    #[error("query operation cancelled")]
    Cancelled,
    #[error("query execution deadline exceeded")]
    DeadlineExceeded,
    #[error("query repository failure: {0}")]
    Repository(#[from] sqlx::Error),
    #[error("query internal failure: {0}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for QueryServiceError {
    fn from(error: anyhow::Error) -> Self {
        let error = match error.downcast::<Self>() {
            Ok(error) => return error,
            Err(error) => error,
        };
        let error = match error.downcast::<ProviderCallError>() {
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
        let error = match error.downcast::<ApiError>() {
            Ok(error) => return error.into(),
            Err(error) => error,
        };
        let error = match error.downcast::<StageError>() {
            Ok(error) => return error.into(),
            Err(error) => error,
        };
        let error = match error.downcast::<sqlx::Error>() {
            Ok(error) => return Self::Repository(error),
            Err(error) => error,
        };
        Self::Internal(error)
    }
}

impl QueryServiceError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::LibraryNotFound { .. } => "QueryServiceError::LibraryNotFound",
            Self::NotFound { .. } => "QueryServiceError::NotFound",
            Self::BindingNotConfigured { .. } => "QueryServiceError::BindingNotConfigured",
            Self::StateConflict { .. } => "QueryServiceError::StateConflict",
            Self::ProviderUnavailable { .. } => "QueryServiceError::ProviderUnavailable",
            Self::CacheUnavailable { .. } => "QueryServiceError::CacheUnavailable",
            Self::Cancelled => "QueryServiceError::Cancelled",
            Self::DeadlineExceeded => "QueryServiceError::DeadlineExceeded",
            Self::Repository(_) => "QueryServiceError::Repository",
            Self::Internal(_) => "QueryServiceError::Internal",
        }
    }

    /// Whether a failure of the chunk-embedding stage should KEEP the chunk
    /// vectors already persisted for this revision instead of wiping them.
    ///
    /// Explicitly typed provider failures leave successfully persisted batches
    /// valid: the next attempt's resume path re-uses them and embeds only the
    /// missing remainder. Repository, cancellation, correctness, and unknown
    /// failures must wipe partial state because reuse may be unsafe.
    #[must_use]
    pub const fn preserves_partial_vectors(&self) -> bool {
        matches!(self, Self::ProviderUnavailable { .. })
    }
}

impl From<ProviderCallError> for QueryServiceError {
    fn from(error: ProviderCallError) -> Self {
        Self::ProviderUnavailable { message: error.to_string() }
    }
}

impl From<IngestServiceError> for QueryServiceError {
    fn from(error: IngestServiceError) -> Self {
        match error {
            IngestServiceError::LibraryNotFound { library_id } => {
                Self::LibraryNotFound { library_id }
            }
            IngestServiceError::BindingNotConfigured { message } => {
                Self::BindingNotConfigured { message }
            }
            IngestServiceError::ProviderUnavailable { message } => {
                Self::ProviderUnavailable { message }
            }
            IngestServiceError::StateConflict { message } => Self::StateConflict { message },
            IngestServiceError::Cancelled => Self::Cancelled,
            IngestServiceError::Repository(error) => Self::Repository(error),
            IngestServiceError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<KnowledgeServiceError> for QueryServiceError {
    fn from(error: KnowledgeServiceError) -> Self {
        match error {
            KnowledgeServiceError::LibraryNotFound { library_id } => {
                Self::LibraryNotFound { library_id }
            }
            KnowledgeServiceError::NotFound { message } => Self::NotFound { message },
            KnowledgeServiceError::GraphNotReady { message } => Self::StateConflict { message },
            KnowledgeServiceError::CacheUnavailable { message } => {
                Self::CacheUnavailable { message }
            }
            KnowledgeServiceError::Repository(error) => Self::Repository(error),
            KnowledgeServiceError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<StageError> for QueryServiceError {
    fn from(_: StageError) -> Self {
        Self::Cancelled
    }
}

impl From<ApiError> for QueryServiceError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::NotFound(message) => Self::NotFound { message },
            ApiError::ProviderFailure(message) => Self::ProviderUnavailable { message },
            ApiError::GatewayTimeout { .. } => Self::DeadlineExceeded,
            ApiError::KnowledgeNotReady(message)
            | ApiError::Conflict(message)
            | ApiError::UnreadableDocument(message)
            | ApiError::StaleRevision(message)
            | ApiError::ConflictingMutation(message)
            | ApiError::BootstrapAlreadyClaimed(message)
            | ApiError::IdempotencyConflict(message)
            | ApiError::MissingPrice(message)
            | ApiError::GraphWriteContention(message)
            | ApiError::GraphPersistenceIntegrity(message) => Self::StateConflict { message },
            ApiError::SettlementRefreshFailed(message) => Self::StateConflict { message },
            other => Self::Internal(anyhow::Error::new(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QueryServiceError;
    use crate::integrations::retry::ProviderCallError;
    use uuid::Uuid;

    // BUG B: only explicitly typed provider failures preserve partial vectors;
    // terminal, repository, and unknown failures must still wipe them.
    #[test]
    fn only_provider_unavailable_preserves_partial_vectors() {
        assert!(
            QueryServiceError::ProviderUnavailable { message: "embedding 429".to_string() }
                .preserves_partial_vectors()
        );
        assert!(!QueryServiceError::Cancelled.preserves_partial_vectors());
        assert!(
            !QueryServiceError::StateConflict { message: "coverage mismatch".to_string() }
                .preserves_partial_vectors()
        );
        assert!(
            !QueryServiceError::Internal(anyhow::anyhow!("dimension mismatch"))
                .preserves_partial_vectors()
        );
        assert!(
            !QueryServiceError::LibraryNotFound { library_id: Uuid::now_v7() }
                .preserves_partial_vectors()
        );
    }

    #[test]
    fn unknown_anyhow_messages_are_always_internal() {
        for message in [
            "library not found",
            "binding not configured",
            "provider embedding upstream timeout",
            "redis cache unavailable",
            "operation cancelled",
            "invalid state conflict",
        ] {
            let error = QueryServiceError::from(anyhow::anyhow!(message));

            assert!(matches!(&error, QueryServiceError::Internal(_)), "{message}");
            assert!(!error.preserves_partial_vectors(), "{message}");
        }
    }

    #[test]
    fn anyhow_context_preserves_typed_provider_failure() {
        let error = anyhow::Error::new(ProviderCallError::protocol("opaque provider failure"))
            .context("query embedding boundary failed");

        let error = QueryServiceError::from(error);
        assert!(matches!(
            &error,
            QueryServiceError::ProviderUnavailable { message }
                if message == "opaque provider failure"
        ));
        assert!(error.preserves_partial_vectors());
    }
}
