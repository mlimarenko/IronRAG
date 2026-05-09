use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum WebhookServiceError {
    #[error("webhook delivery attempt for job {job_id} not found")]
    DeliveryAttemptNotFound { job_id: Uuid },
    #[error("webhook subscription {subscription_id} not found")]
    SubscriptionNotFound { subscription_id: Uuid },
    #[error("webhook state conflict: {message}")]
    StateConflict { message: String },
    #[error("webhook repository failure: {0}")]
    Repository(#[from] sqlx::Error),
    #[error("webhook internal failure: {0}")]
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for WebhookServiceError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl WebhookServiceError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::DeliveryAttemptNotFound { .. } => "WebhookServiceError::DeliveryAttemptNotFound",
            Self::SubscriptionNotFound { .. } => "WebhookServiceError::SubscriptionNotFound",
            Self::StateConflict { .. } => "WebhookServiceError::StateConflict",
            Self::Repository(_) => "WebhookServiceError::Repository",
            Self::Internal(_) => "WebhookServiceError::Internal",
        }
    }
}
