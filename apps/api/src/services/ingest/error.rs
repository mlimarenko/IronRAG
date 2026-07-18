use thiserror::Error;
use uuid::Uuid;

use crate::{
    interfaces::http::router_support::ApiError, services::ingest::cancellation::StageError,
};

#[derive(Debug, Error)]
pub enum IngestServiceError {
    #[error("library {library_id} not found")]
    LibraryNotFound { library_id: Uuid },
    #[error("ingest binding not configured: {message}")]
    BindingNotConfigured { message: String },
    #[error("ingest state conflict: {message}")]
    StateConflict { message: String },
    #[error("ingest provider unavailable: {message}")]
    ProviderUnavailable { message: String },
    #[error("ingest operation cancelled")]
    Cancelled,
    #[error("ingest repository failure: {0}")]
    Repository(#[from] sqlx::Error),
    #[error("ingest internal failure: {0}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for IngestServiceError {
    fn from(error: anyhow::Error) -> Self {
        let error = match error.downcast::<Self>() {
            Ok(error) => return error,
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

impl IngestServiceError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::LibraryNotFound { .. } => "IngestServiceError::LibraryNotFound",
            Self::BindingNotConfigured { .. } => "IngestServiceError::BindingNotConfigured",
            Self::StateConflict { .. } => "IngestServiceError::StateConflict",
            Self::ProviderUnavailable { .. } => "IngestServiceError::ProviderUnavailable",
            Self::Cancelled => "IngestServiceError::Cancelled",
            Self::Repository(_) => "IngestServiceError::Repository",
            Self::Internal(_) => "IngestServiceError::Internal",
        }
    }
}

impl From<ApiError> for IngestServiceError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::NotFound(message) => Self::StateConflict { message },
            ApiError::Conflict(message)
            | ApiError::BootstrapAlreadyClaimed(message)
            | ApiError::UnreadableDocument(message)
            | ApiError::StaleRevision(message)
            | ApiError::ConflictingMutation(message)
            | ApiError::IdempotencyConflict(message)
            | ApiError::MissingPrice(message)
            | ApiError::KnowledgeNotReady(message)
            | ApiError::GraphWriteContention(message)
            | ApiError::GraphPersistenceIntegrity(message)
            | ApiError::SettlementRefreshFailed(message) => Self::StateConflict { message },
            ApiError::ProviderFailure(message) => Self::ProviderUnavailable { message },
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
        }
    }
}

impl From<StageError> for IngestServiceError {
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
            "binding not configured",
            "provider unavailable",
            "operation cancelled",
            "invalid state conflict",
        ] {
            let error = IngestServiceError::from(anyhow::anyhow!(message));

            assert!(matches!(error, IngestServiceError::Internal(_)), "{message}");
        }
    }

    #[test]
    fn anyhow_context_preserves_typed_api_error() {
        let error = anyhow::Error::new(ApiError::ProviderFailure("opaque".to_string()))
            .context("ingest boundary failed");

        assert!(matches!(
            IngestServiceError::from(error),
            IngestServiceError::ProviderUnavailable { message } if message == "opaque"
        ));
    }

    #[test]
    fn anyhow_context_preserves_typed_repository_error() {
        let error = anyhow::Error::new(sqlx::Error::RowNotFound).context("ingest query failed");

        assert!(matches!(
            IngestServiceError::from(error),
            IngestServiceError::Repository(sqlx::Error::RowNotFound)
        ));
    }
}
