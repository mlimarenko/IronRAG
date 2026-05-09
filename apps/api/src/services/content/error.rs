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
        let message = error.to_string();
        match Self::from_message(message) {
            Self::InvalidRequest { .. } => Self::Internal(error),
            classified => classified,
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

    #[must_use]
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let normalized = message.to_ascii_lowercase();
        if normalized.contains("not found") || normalized.contains("no delivery attempt found") {
            return Self::NotFound { message };
        }
        if normalized.contains("already exists")
            || normalized.contains("conflict")
            || normalized.contains("stale revision")
            || normalized.contains("still processing")
        {
            return Self::StateConflict { message };
        }
        if normalized.contains("provider")
            || normalized.contains("llm")
            || normalized.contains("embedding")
            || normalized.contains("upstream")
        {
            return Self::ProviderUnavailable { message };
        }
        if normalized.contains("storage")
            || normalized.contains("object storage")
            || normalized.contains("s3")
            || normalized.contains("stored content")
            || normalized.contains("filesystem")
        {
            return Self::StorageUnavailable { message };
        }
        if normalized.contains("cancelled") || normalized.contains("canceled") {
            return Self::Cancelled;
        }
        Self::InvalidRequest { message }
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
            ApiError::ArangoBootstrapFailed(message) => Self::StorageUnavailable { message },
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
