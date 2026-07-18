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
        let error = match error.downcast::<Self>() {
            Ok(error) => return error,
            Err(error) => error,
        };
        match error.downcast::<sqlx::Error>() {
            Ok(error) => Self::Repository(error),
            Err(error) => Self::Internal(error),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_anyhow_messages_are_always_internal() {
        for message in ["library not found", "graph is ready", "redis cache unavailable"] {
            let error = KnowledgeServiceError::from(anyhow::anyhow!(message));

            assert!(matches!(error, KnowledgeServiceError::Internal(_)), "{message}");
        }
    }

    #[test]
    fn anyhow_context_preserves_typed_service_error() {
        let error = anyhow::Error::new(KnowledgeServiceError::GraphNotReady {
            message: "opaque".to_string(),
        })
        .context("knowledge boundary");

        assert!(matches!(
            KnowledgeServiceError::from(error),
            KnowledgeServiceError::GraphNotReady { message } if message == "opaque"
        ));
    }

    #[test]
    fn anyhow_context_preserves_typed_repository_error() {
        let error = anyhow::Error::new(sqlx::Error::RowNotFound).context("knowledge query failed");

        assert!(matches!(
            KnowledgeServiceError::from(error),
            KnowledgeServiceError::Repository(sqlx::Error::RowNotFound)
        ));
    }
}
