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
        let message = error.to_string();
        match Self::from_message(message) {
            Self::Internal(_) => Self::Internal(error),
            classified => classified,
        }
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

    #[must_use]
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let normalized = message.to_ascii_lowercase();
        if normalized.contains("not found") && normalized.contains("library") {
            return Self::StateConflict { message };
        }
        if normalized.contains("not configured") || normalized.contains("no active") {
            return Self::BindingNotConfigured { message };
        }
        if normalized.contains("provider")
            || normalized.contains("llm")
            || normalized.contains("embedding")
            || normalized.contains("upstream")
            || normalized.contains("invalid model output")
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
