use thiserror::Error;
use uuid::Uuid;

use crate::{
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
    #[error("query repository failure: {0}")]
    Repository(#[from] sqlx::Error),
    #[error("query internal failure: {0}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for QueryServiceError {
    fn from(error: anyhow::Error) -> Self {
        let message = error.to_string();
        match Self::from_message(message) {
            Self::Internal(_) => Self::Internal(error),
            classified => classified,
        }
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
            Self::Repository(_) => "QueryServiceError::Repository",
            Self::Internal(_) => "QueryServiceError::Internal",
        }
    }

    /// Whether a failure of the chunk-embedding stage should KEEP the chunk
    /// vectors already persisted for this revision instead of wiping them.
    ///
    /// Transient/retryable failures (provider 429/timeout, vector-store write
    /// blip) leave the successfully persisted batches valid: the next attempt's
    /// resume path (`load_current_revision_chunk_vector_ids`) re-uses them and
    /// only embeds the missing remainder, so a blip at 95% of a large document
    /// no longer throws away every paid-for vector. Terminal/correctness
    /// failures (cancellation, dimension/coverage violations) must still wipe
    /// the partial state because reusing it on a later run could be wrong.
    #[must_use]
    pub const fn preserves_partial_vectors(&self) -> bool {
        matches!(self, Self::ProviderUnavailable { .. })
    }

    #[must_use]
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let normalized = message.to_ascii_lowercase();
        if normalized.contains("not found") {
            return Self::NotFound { message };
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
        if normalized.contains("redis") || normalized.contains("cache") {
            return Self::CacheUnavailable { message };
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
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
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
            other => Self::Internal(anyhow::anyhow!(other.to_string())),
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
            ApiError::KnowledgeNotReady(message)
            | ApiError::Conflict(message)
            | ApiError::UnreadableDocument(message)
            | ApiError::StaleRevision(message)
            | ApiError::ConflictingMutation(message)
            | ApiError::GraphWriteContention(message)
            | ApiError::GraphPersistenceIntegrity(message) => Self::StateConflict { message },
            ApiError::Internal => {
                Self::Internal(anyhow::anyhow!("query dependency returned internal error"))
            }
            other => Self::from_message(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QueryServiceError;
    use uuid::Uuid;

    // BUG B: only transient/retryable provider failures preserve the partial
    // vectors already persisted for a revision; terminal/correctness failures
    // must still wipe them so stale partial state is never reused.
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

    // A transient embedding-provider error message classifies as
    // ProviderUnavailable, so the embed failure path preserves partial vectors.
    #[test]
    fn transient_embedding_error_classifies_as_provider_unavailable() {
        let error = QueryServiceError::from_message(
            "failed to embed chunk batch for revision: upstream timeout",
        );
        assert!(matches!(error, QueryServiceError::ProviderUnavailable { .. }));
        assert!(error.preserves_partial_vectors());
    }
}
