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
        let message = error.to_string();
        match Self::from_message(message) {
            Self::Internal(_) => Self::Internal(error),
            classified => classified,
        }
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

    #[must_use]
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let normalized = message.to_ascii_lowercase();
        if normalized.contains("library") && normalized.contains("not found") {
            return Self::NotFound { message };
        }
        if normalized.contains("not found") {
            return Self::NotFound { message };
        }
        if normalized.contains("graph write contention")
            || normalized.contains("projection contention")
            || normalized.contains("deadlock")
            || normalized.contains("lock timeout")
        {
            return Self::WriteContention { message };
        }
        if normalized.contains("graph persistence integrity")
            || normalized.contains("foreign key violation")
            || normalized.contains("edge persistence skipped because node")
        {
            return Self::PersistenceIntegrity { message };
        }
        if normalized.contains("provider")
            || normalized.contains("llm")
            || normalized.contains("embedding")
            || normalized.contains("upstream")
        {
            return Self::ProviderUnavailable { message };
        }
        if normalized.contains("cancelled") || normalized.contains("canceled") {
            return Self::Cancelled;
        }
        if normalized.contains("conflict") || normalized.contains("invalid state") {
            return Self::StateConflict { message };
        }
        Self::Internal(anyhow::anyhow!(message))
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
