//! One shared, cloneable classification of a terminal provider failure.
//!
//! [`crate::integrations::retry::ProviderCallError`] carries rich sources
//! (`reqwest::Error`, upstream response bodies) that are neither cloneable nor
//! safe to persist. Service enums used to flatten it into a bare `String`
//! message at each conversion boundary, which lost the machine-readable
//! classification the operator-facing redacted storage needs. This type is the
//! sanitized snapshot taken once at that boundary: the failure kind, the
//! existing human message (Display is byte-identical to the flattened string
//! it replaces), and the value-free redacted detail token.

use crate::integrations::retry::ProviderCallError;

/// Terminal classification a provider failure resolves to. Quota exhaustion is
/// its own kind because it must never be retried: unlike a rate limit it does
/// not heal on a retry timescale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureKind {
    Unavailable,
    QuotaExhausted,
}

/// Bounded, value-free failure detail. The only constructors are private and
/// apply the identifier alphabet filter, so free-form diagnostics (which may
/// carry upstream response text) cannot enter operator-facing storage through
/// this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedFailureDetail(String);

const MAX_REDACTED_DETAIL_CHARS: usize = 120;

impl RedactedFailureDetail {
    fn sanitized(detail: &str) -> Option<Self> {
        let sanitized: String = detail
            .chars()
            .filter(|ch| {
                ch.is_ascii_lowercase()
                    || ch.is_ascii_digit()
                    || matches!(ch, '_' | ' ' | '=' | '-')
            })
            .take(MAX_REDACTED_DETAIL_CHARS)
            .collect();
        let trimmed = sanitized.trim();
        if trimmed.is_empty() { None } else { Some(Self(trimmed.to_string())) }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Cloneable, sanitized snapshot of one provider failure. `Display` emits only
/// the message so every existing flattened-string surface stays byte-identical.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProviderFailure {
    kind: ProviderFailureKind,
    message: String,
    detail_redacted: Option<RedactedFailureDetail>,
}

impl ProviderFailure {
    /// Snapshot for failures that never had a typed provider error (for
    /// example an agent-loop stop reason that only carries prose). No detail
    /// token is invented for them.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderFailureKind::Unavailable,
            message: message.into(),
            detail_redacted: None,
        }
    }

    #[must_use]
    pub fn quota_exhausted(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderFailureKind::QuotaExhausted,
            message: message.into(),
            detail_redacted: None,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ProviderFailureKind {
        self.kind
    }

    /// Stable label the service-enum Display strings interpolate; existing
    /// flattened messages depend on these exact words.
    #[must_use]
    pub const fn kind_label(&self) -> &'static str {
        match self.kind {
            ProviderFailureKind::Unavailable => "unavailable",
            ProviderFailureKind::QuotaExhausted => "quota exhausted",
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn detail_redacted(&self) -> Option<&str> {
        self.detail_redacted.as_ref().map(RedactedFailureDetail::as_str)
    }
}

impl From<&ProviderCallError> for ProviderFailure {
    fn from(error: &ProviderCallError) -> Self {
        let kind = if error.is_quota_exhausted() {
            ProviderFailureKind::QuotaExhausted
        } else {
            ProviderFailureKind::Unavailable
        };
        Self {
            kind,
            message: error.to_string(),
            detail_redacted: RedactedFailureDetail::sanitized(&error.redacted_detail()),
        }
    }
}

impl From<ProviderCallError> for ProviderFailure {
    fn from(error: ProviderCallError) -> Self {
        Self::from(&error)
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;

    use super::*;
    use crate::integrations::retry::provider_http_status_error;

    #[test]
    fn snapshot_preserves_the_flattened_message_and_classifies_quota() {
        let headers = HeaderMap::new();
        let body = r#"{"error":{"message":"secret-body","code":"insufficient_quota"}}"#;
        let error =
            provider_http_status_error("openai", StatusCode::TOO_MANY_REQUESTS, &headers, body);
        let flattened = error.to_string();

        let failure = ProviderFailure::from(&error);

        assert_eq!(failure.kind(), ProviderFailureKind::QuotaExhausted);
        assert_eq!(failure.to_string(), flattened);
        assert_eq!(
            failure.detail_redacted(),
            Some("provider=openai http_status=429 quota_exhausted")
        );
    }

    #[test]
    fn prose_constructors_carry_no_invented_detail() {
        let failure = ProviderFailure::unavailable("agent loop stopped on provider error");

        assert_eq!(failure.kind(), ProviderFailureKind::Unavailable);
        assert_eq!(failure.detail_redacted(), None);
        assert_eq!(failure.to_string(), "agent loop stopped on provider error");
    }

    #[test]
    fn detail_sanitizer_is_defense_in_depth_not_a_redactor() {
        let hostile = ProviderCallError::protocol("Secret! Диагностика с UPPER and symbols %$");
        let failure = ProviderFailure::from(&hostile);

        // Protocol variants classify by shape only; the hostile message never
        // reaches the detail token.
        assert_eq!(failure.detail_redacted(), Some("protocol"));
    }
}
