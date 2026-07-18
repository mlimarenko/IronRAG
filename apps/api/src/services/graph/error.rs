use thiserror::Error;
use uuid::Uuid;

use crate::{
    interfaces::http::router_support::ApiError,
    services::{
        ingest::{cancellation::StageError, error::IngestServiceError},
        query::error::QueryServiceError,
    },
};

#[derive(Debug, Error)]
pub enum GraphServiceError {
    #[error("library {library_id} not found")]
    LibraryNotFound { library_id: Uuid },
    #[error("graph resource not found: {message}")]
    NotFound { message: String },
    #[error("graph state conflict: {message}")]
    StateConflict { message: String },
    #[error("graph write contention: {message}")]
    WriteContention { message: String },
    #[error("graph persistence integrity failure: {message}")]
    PersistenceIntegrity { message: String },
    #[error("graph provider unavailable: {message}")]
    ProviderUnavailable { message: String },
    #[error("graph operation cancelled")]
    Cancelled,
    #[error("graph repository failure: {0}")]
    Repository(#[from] sqlx::Error),
    #[error("graph internal failure: {0}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for GraphServiceError {
    fn from(error: anyhow::Error) -> Self {
        let error = match error.downcast::<Self>() {
            Ok(error) => return error,
            Err(error) => error,
        };
        let error = match error.downcast::<IngestServiceError>() {
            Ok(error) => return error.into(),
            Err(error) => error,
        };
        let error = match error.downcast::<QueryServiceError>() {
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

impl GraphServiceError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::LibraryNotFound { .. } => "GraphServiceError::LibraryNotFound",
            Self::NotFound { .. } => "GraphServiceError::NotFound",
            Self::StateConflict { .. } => "GraphServiceError::StateConflict",
            Self::WriteContention { .. } => "GraphServiceError::WriteContention",
            Self::PersistenceIntegrity { .. } => "GraphServiceError::PersistenceIntegrity",
            Self::ProviderUnavailable { .. } => "GraphServiceError::ProviderUnavailable",
            Self::Cancelled => "GraphServiceError::Cancelled",
            Self::Repository(_) => "GraphServiceError::Repository",
            Self::Internal(_) => "GraphServiceError::Internal",
        }
    }
}

impl From<IngestServiceError> for GraphServiceError {
    fn from(error: IngestServiceError) -> Self {
        match error {
            IngestServiceError::LibraryNotFound { library_id } => {
                Self::LibraryNotFound { library_id }
            }
            IngestServiceError::ProviderUnavailable { message } => {
                Self::ProviderUnavailable { message }
            }
            IngestServiceError::BindingNotConfigured { message }
            | IngestServiceError::StateConflict { message } => Self::StateConflict { message },
            IngestServiceError::Cancelled => Self::Cancelled,
            IngestServiceError::Repository(error) => Self::Repository(error),
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
        }
    }
}

impl From<ApiError> for GraphServiceError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::NotFound(message) => Self::NotFound { message },
            ApiError::Conflict(message)
            | ApiError::BootstrapAlreadyClaimed(message)
            | ApiError::UnreadableDocument(message)
            | ApiError::StaleRevision(message)
            | ApiError::ConflictingMutation(message)
            | ApiError::IdempotencyConflict(message)
            | ApiError::MissingPrice(message)
            | ApiError::KnowledgeNotReady(message)
            | ApiError::SettlementRefreshFailed(message) => Self::StateConflict { message },
            ApiError::GraphWriteContention(message) => Self::WriteContention { message },
            ApiError::GraphPersistenceIntegrity(message) => Self::PersistenceIntegrity { message },
            ApiError::ProviderFailure(message) => Self::ProviderUnavailable { message },
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
        }
    }
}

impl From<QueryServiceError> for GraphServiceError {
    fn from(error: QueryServiceError) -> Self {
        match error {
            QueryServiceError::LibraryNotFound { library_id } => {
                Self::LibraryNotFound { library_id }
            }
            QueryServiceError::NotFound { message } => Self::NotFound { message },
            QueryServiceError::BindingNotConfigured { message }
            | QueryServiceError::StateConflict { message }
            | QueryServiceError::CacheUnavailable { message } => Self::StateConflict { message },
            QueryServiceError::ProviderUnavailable { message } => {
                Self::ProviderUnavailable { message }
            }
            QueryServiceError::Cancelled => Self::Cancelled,
            QueryServiceError::DeadlineExceeded => Self::Cancelled,
            QueryServiceError::Repository(error) => Self::Repository(error),
            QueryServiceError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<StageError> for GraphServiceError {
    fn from(_: StageError) -> Self {
        Self::Cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_anyhow_messages_are_always_internal() {
        for message in [
            "library not found",
            "graph write contention",
            "foreign key violation",
            "provider unavailable",
            "operation cancelled",
            "invalid state conflict",
        ] {
            let error = GraphServiceError::from(anyhow::anyhow!(message));

            assert!(matches!(error, GraphServiceError::Internal(_)), "{message}");
        }
    }

    #[test]
    fn anyhow_context_preserves_typed_api_error() {
        let error = anyhow::Error::new(ApiError::GraphWriteContention("opaque".to_string()))
            .context("graph boundary failed");

        assert!(matches!(
            GraphServiceError::from(error),
            GraphServiceError::WriteContention { message } if message == "opaque"
        ));
    }

    #[test]
    fn anyhow_context_preserves_typed_repository_error() {
        let error = anyhow::Error::new(sqlx::Error::RowNotFound).context("graph query failed");

        assert!(matches!(
            GraphServiceError::from(error),
            GraphServiceError::Repository(sqlx::Error::RowNotFound)
        ));
    }
}
