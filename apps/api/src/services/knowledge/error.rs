use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum KnowledgeServiceError {
    #[error("library {library_id} not found")]
    LibraryNotFound { library_id: Uuid },
    #[error("knowledge resource not found: {message}")]
    NotFound { message: String },
    #[error("knowledge graph not ready: {message}")]
    GraphNotReady { message: String },
    #[error("knowledge cache unavailable: {message}")]
    CacheUnavailable { message: String },
    #[error("knowledge repository failure: {0}")]
    Repository(#[from] sqlx::Error),
    #[error("knowledge internal failure: {0}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for KnowledgeServiceError {
    fn from(error: anyhow::Error) -> Self {
        let message = error.to_string();
        match Self::from_message(message) {
            Self::Internal(_) => Self::Internal(error),
            classified => classified,
        }
    }
}

impl KnowledgeServiceError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::LibraryNotFound { .. } => "KnowledgeServiceError::LibraryNotFound",
            Self::NotFound { .. } => "KnowledgeServiceError::NotFound",
            Self::GraphNotReady { .. } => "KnowledgeServiceError::GraphNotReady",
            Self::CacheUnavailable { .. } => "KnowledgeServiceError::CacheUnavailable",
            Self::Repository(_) => "KnowledgeServiceError::Repository",
            Self::Internal(_) => "KnowledgeServiceError::Internal",
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
        if normalized.contains("graph") && normalized.contains("ready") {
            return Self::GraphNotReady { message };
        }
        if normalized.contains("redis") || normalized.contains("cache") {
            return Self::CacheUnavailable { message };
        }
        Self::Internal(anyhow::anyhow!(message))
    }
}
