use std::fmt;

use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::shared::secret_encryption::SecretEncryptionError;

/// Stable, non-sensitive classification persisted for a failed delivery.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WebhookDeliveryFailureCode {
    SubscriptionInactive,
    TargetPolicyRejected,
    TargetResolutionFailed,
    PayloadEncodingFailed,
    CredentialUnavailable,
    ClientSetupFailed,
    TransportTimeout,
    TransportConnect,
    TransportRequest,
    RemoteHttpStatus,
}

impl WebhookDeliveryFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubscriptionInactive => "subscription_inactive",
            Self::TargetPolicyRejected => "target_policy_rejected",
            Self::TargetResolutionFailed => "target_resolution_failed",
            Self::PayloadEncodingFailed => "payload_encoding_failed",
            Self::CredentialUnavailable => "credential_unavailable",
            Self::ClientSetupFailed => "client_setup_failed",
            Self::TransportTimeout => "transport_timeout",
            Self::TransportConnect => "transport_connect",
            Self::TransportRequest => "transport_request",
            Self::RemoteHttpStatus => "remote_http_status",
        }
    }
}

impl fmt::Debug for WebhookDeliveryFailureCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for WebhookDeliveryFailureCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A bounded delivery failure safe for logs and persistence.
///
/// Summaries are intentionally static: accepting a reqwest error string here
/// would make it possible to persist or log a target URL, path, or query.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct WebhookDeliveryFailure {
    code: WebhookDeliveryFailureCode,
}

impl WebhookDeliveryFailure {
    #[must_use]
    pub const fn new(code: WebhookDeliveryFailureCode) -> Self {
        Self { code }
    }

    #[must_use]
    pub const fn code(self) -> WebhookDeliveryFailureCode {
        self.code
    }

    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self.code {
            WebhookDeliveryFailureCode::SubscriptionInactive => "Webhook subscription is inactive",
            WebhookDeliveryFailureCode::TargetPolicyRejected => {
                "Webhook target was rejected by outbound network policy"
            }
            WebhookDeliveryFailureCode::TargetResolutionFailed => {
                "Webhook target could not be resolved to an allowed endpoint"
            }
            WebhookDeliveryFailureCode::PayloadEncodingFailed => {
                "Webhook payload could not be encoded"
            }
            WebhookDeliveryFailureCode::CredentialUnavailable => {
                "Protected webhook credentials could not be loaded"
            }
            WebhookDeliveryFailureCode::ClientSetupFailed => {
                "Outbound webhook client could not be initialized"
            }
            WebhookDeliveryFailureCode::TransportTimeout => "Outbound webhook request timed out",
            WebhookDeliveryFailureCode::TransportConnect => {
                "Outbound webhook endpoint could not be reached"
            }
            WebhookDeliveryFailureCode::TransportRequest => "Outbound webhook request failed",
            WebhookDeliveryFailureCode::RemoteHttpStatus => {
                "Remote endpoint returned an unsuccessful HTTP status"
            }
        }
    }
}

impl fmt::Debug for WebhookDeliveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookDeliveryFailure")
            .field("code", &self.code)
            .field("summary", &self.summary())
            .finish()
    }
}

impl fmt::Display for WebhookDeliveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.summary())
    }
}

#[derive(Error)]
pub enum WebhookServiceError {
    #[error("webhook delivery attempt for job {job_id} not found")]
    DeliveryAttemptNotFound { job_id: Uuid },
    #[error("webhook subscription {subscription_id} not found")]
    SubscriptionNotFound { subscription_id: Uuid },
    #[error("webhook delivery lease is still in flight until {retry_at}")]
    DeliveryLeaseInFlight { attempt_id: Uuid, job_id: Uuid, retry_at: DateTime<Utc> },
    #[error("webhook delivery execution was canceled")]
    DeliveryCanceled { attempt_id: Uuid, job_id: Uuid },
    #[error("webhook state conflict")]
    StateConflict { message: String },
    #[error("webhook repository operation failed")]
    Repository(#[from] sqlx::Error),
    #[error("webhook credential protection operation failed")]
    CredentialProtection(#[from] SecretEncryptionError),
    #[error("webhook internal operation failed")]
    Internal(#[source] anyhow::Error),
}

impl fmt::Debug for WebhookServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("WebhookServiceError");
        debug.field("kind", &self.kind());
        match self {
            Self::DeliveryAttemptNotFound { job_id } => debug.field("job_id", job_id),
            Self::SubscriptionNotFound { subscription_id } => {
                debug.field("subscription_id", subscription_id)
            }
            Self::DeliveryLeaseInFlight { attempt_id, job_id, retry_at } => debug
                .field("attempt_id", attempt_id)
                .field("job_id", job_id)
                .field("retry_at", retry_at),
            Self::DeliveryCanceled { attempt_id, job_id } => {
                debug.field("attempt_id", attempt_id).field("job_id", job_id)
            }
            Self::StateConflict { .. }
            | Self::Repository(_)
            | Self::CredentialProtection(_)
            | Self::Internal(_) => debug.field("detail", &"<redacted>"),
        };
        debug.finish()
    }
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
            Self::DeliveryLeaseInFlight { .. } => "WebhookServiceError::DeliveryLeaseInFlight",
            Self::DeliveryCanceled { .. } => "WebhookServiceError::DeliveryCanceled",
            Self::StateConflict { .. } => "WebhookServiceError::StateConflict",
            Self::Repository(_) => "WebhookServiceError::Repository",
            Self::CredentialProtection(_) => "WebhookServiceError::CredentialProtection",
            Self::Internal(_) => "WebhookServiceError::Internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WebhookDeliveryFailure, WebhookDeliveryFailureCode, WebhookServiceError};

    #[test]
    fn delivery_failure_is_stable_and_contains_no_dynamic_target_detail() {
        let failure = WebhookDeliveryFailure::new(WebhookDeliveryFailureCode::TransportTimeout);

        assert_eq!(failure.code().as_str(), "transport_timeout");
        assert_eq!(failure.summary(), "Outbound webhook request timed out");
        assert_eq!(failure.to_string(), "transport_timeout: Outbound webhook request timed out");
    }

    #[test]
    fn service_error_debug_redacts_internal_error_detail() {
        let secret_url = "https://example.invalid/private/path?token=synthetic-secret";
        let error = WebhookServiceError::Internal(anyhow::anyhow!(secret_url));

        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(!debug.contains(secret_url));
        assert!(!display.contains(secret_url));
        assert!(debug.contains("<redacted>"));
    }
}
