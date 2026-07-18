use thiserror::Error;

use crate::{
    interfaces::http::router_support::ApiError,
    services::{
        graph::error::GraphServiceError,
        ingest::{cancellation::StageError, error::IngestServiceError},
        knowledge::error::KnowledgeServiceError,
    },
};

#[derive(Debug, Error)]
pub enum ContentServiceError {
    #[error("content resource not found: {message}")]
    NotFound { message: String },
    #[error("content request invalid: {message}")]
    InvalidRequest { message: String },
    #[error("content state conflict: {message}")]
    StateConflict { message: String },
    #[error("content storage unavailable: {message}")]
    StorageUnavailable { message: String },
    #[error("content provider unavailable: {message}")]
    ProviderUnavailable { message: String },
    #[error("content operation cancelled")]
    Cancelled,
    #[error("content repository failure: {0}")]
    Repository(#[from] sqlx::Error),
    #[error("content internal failure: {0}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ContentServiceError {
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
        let error = match error.downcast::<IngestServiceError>() {
            Ok(error) => return error.into(),
            Err(error) => error,
        };
        let error = match error.downcast::<GraphServiceError>() {
            Ok(error) => return error.into(),
            Err(error) => error,
        };
        let error = match error.downcast::<KnowledgeServiceError>() {
            Ok(error) => return error.into(),
            Err(error) => error,
        };
        match error.downcast::<sqlx::Error>() {
            Ok(error) => Self::Repository(error),
            Err(error) => Self::Internal(error),
        }
    }
}

impl ContentServiceError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "ContentServiceError::NotFound",
            Self::InvalidRequest { .. } => "ContentServiceError::InvalidRequest",
            Self::StateConflict { .. } => "ContentServiceError::StateConflict",
            Self::StorageUnavailable { .. } => "ContentServiceError::StorageUnavailable",
            Self::ProviderUnavailable { .. } => "ContentServiceError::ProviderUnavailable",
            Self::Cancelled => "ContentServiceError::Cancelled",
            Self::Repository(_) => "ContentServiceError::Repository",
            Self::Internal(_) => "ContentServiceError::Internal",
        }
    }
}

impl From<ApiError> for ContentServiceError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::NotFound(message) => Self::NotFound { message },
            ApiError::BadRequest(message)
            | ApiError::InvalidMcpToolCall(message)
            | ApiError::InvalidContinuationToken(message) => Self::InvalidRequest { message },
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
            ApiError::ServiceUnavailable { message, .. } => Self::StorageUnavailable { message },
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
        }
    }
}

impl From<StageError> for ContentServiceError {
    fn from(_: StageError) -> Self {
        Self::Cancelled
    }
}

impl From<IngestServiceError> for ContentServiceError {
    fn from(error: IngestServiceError) -> Self {
        match error {
            IngestServiceError::LibraryNotFound { library_id } => {
                Self::NotFound { message: format!("library {library_id} not found") }
            }
            IngestServiceError::BindingNotConfigured { message }
            | IngestServiceError::StateConflict { message } => Self::StateConflict { message },
            IngestServiceError::ProviderUnavailable { message } => {
                Self::ProviderUnavailable { message }
            }
            IngestServiceError::Cancelled => Self::Cancelled,
            IngestServiceError::Repository(error) => Self::Repository(error),
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
        }
    }
}

impl From<GraphServiceError> for ContentServiceError {
    fn from(error: GraphServiceError) -> Self {
        match error {
            GraphServiceError::LibraryNotFound { library_id } => {
                Self::NotFound { message: format!("library {library_id} not found") }
            }
            GraphServiceError::NotFound { message } => Self::NotFound { message },
            GraphServiceError::StateConflict { message }
            | GraphServiceError::WriteContention { message }
            | GraphServiceError::PersistenceIntegrity { message } => {
                Self::StateConflict { message }
            }
            GraphServiceError::ProviderUnavailable { message } => {
                Self::ProviderUnavailable { message }
            }
            GraphServiceError::Cancelled => Self::Cancelled,
            GraphServiceError::Repository(error) => Self::Repository(error),
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
        }
    }
}

impl From<KnowledgeServiceError> for ContentServiceError {
    fn from(error: KnowledgeServiceError) -> Self {
        match error {
            KnowledgeServiceError::LibraryNotFound { library_id } => {
                Self::NotFound { message: format!("library {library_id} not found") }
            }
            KnowledgeServiceError::NotFound { message } => Self::NotFound { message },
            KnowledgeServiceError::GraphNotReady { message } => Self::StateConflict { message },
            KnowledgeServiceError::CacheUnavailable { message } => {
                Self::StorageUnavailable { message }
            }
            KnowledgeServiceError::Repository(error) => Self::Repository(error),
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_anyhow_messages_are_always_internal() {
        for message in [
            "resource not found",
            "state conflict",
            "provider upstream failure",
            "object storage unavailable",
            "operation cancelled",
        ] {
            let error = ContentServiceError::from(anyhow::anyhow!(message));

            assert!(matches!(error, ContentServiceError::Internal(_)), "{message}");
        }
    }

    #[test]
    fn anyhow_context_preserves_typed_api_error() {
        let error = anyhow::Error::new(ApiError::ProviderFailure("opaque".to_string()))
            .context("content boundary");

        assert!(matches!(
            ContentServiceError::from(error),
            ContentServiceError::ProviderUnavailable { message } if message == "opaque"
        ));
    }

    #[test]
    fn anyhow_context_preserves_typed_graph_error() {
        let error = anyhow::Error::new(GraphServiceError::PersistenceIntegrity {
            message: "opaque".to_string(),
        })
        .context("content graph boundary");

        assert!(matches!(
            ContentServiceError::from(error),
            ContentServiceError::StateConflict { message } if message == "opaque"
        ));
    }

    #[test]
    fn anyhow_context_preserves_typed_repository_error() {
        let error = anyhow::Error::new(sqlx::Error::RowNotFound).context("content query failed");

        assert!(matches!(
            ContentServiceError::from(error),
            ContentServiceError::Repository(sqlx::Error::RowNotFound)
        ));
    }
}
