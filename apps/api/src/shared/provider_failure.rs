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
//!
//! Local admission is part of that classification. A dispatch the durable
//! admission owner refused never left the process, so it is neither a provider
//! outage nor a dispatch that happened: it keeps its own kinds and the
//! value-free refusal token ([`ProviderFailure::refusal_code`]) instead of
//! collapsing into [`ProviderFailureKind::Unavailable`]. No variant or token
//! carries a provider response body, a credential or an endpoint.

use crate::integrations::{provider_budget::ProviderBudgetError, retry::ProviderCallError};
use crate::shared::provider_http::ProviderDispatchRefusal;

/// Terminal classification a provider failure resolves to.
///
/// [`Self::QuotaExhausted`], [`Self::AdmissionRefused`] and
/// [`Self::AllowanceExhausted`] must never be retried: unlike a rate limit they
/// do not heal on a retry timescale, and repeating them either burns attempt
/// budget or evades a financial guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureKind {
    /// The provider could not be reached, or the local dispatch capacity could
    /// not admit the attempt yet. Operational and self-healing.
    Unavailable,
    /// The provider formally reported exhausted billing quota.
    QuotaExhausted,
    /// The provider rejected this request as malformed or unsupported.
    RequestRejected,
    /// The durable dispatch owner refused the attempt before any byte left:
    /// unauthorized configuration, route or credential mismatch, an
    /// out-of-envelope request, an uninspectable request body, or a local
    /// admission that could never reserve the attempt.
    AdmissionRefused,
    /// The financial allowance of the resolved scope is exhausted. Confirmed by
    /// the admission owner, not guessed from provider text.
    AllowanceExhausted,
}

/// Bounded, value-free failure detail. The only constructors are private and
/// apply the identifier alphabet filter, so free-form diagnostics (which may
/// carry upstream response text) cannot enter operator-facing storage through
/// this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedFailureDetail(String);

const MAX_REDACTED_DETAIL_CHARS: usize = 200;

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

    /// Snapshot for a refusal the caller can describe but did not receive as a
    /// typed admission error. The refusal token is intentionally absent: this
    /// constructor cannot invent one.
    #[must_use]
    pub fn admission_refused(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderFailureKind::AdmissionRefused,
            message: message.into(),
            detail_redacted: None,
        }
    }

    /// Snapshot for a confirmed exhausted financial allowance that arrived
    /// without a typed admission error.
    #[must_use]
    pub fn allowance_exhausted(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderFailureKind::AllowanceExhausted,
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
            ProviderFailureKind::RequestRejected => "request rejected",
            ProviderFailureKind::AdmissionRefused => "admission refused",
            ProviderFailureKind::AllowanceExhausted => "allowance exhausted",
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

    /// Value-free subreason of a local dispatch refusal, e.g.
    /// `dispatch_authority`, `dispatch_envelope` or `dispatch_exhausted`.
    ///
    /// `None` for every failure that never reached the local admission owner,
    /// so a caller can tell "the provider answered badly" from "we refused to
    /// dispatch" without parsing message text.
    #[must_use]
    pub fn refusal_code(&self) -> Option<&str> {
        if self.is_refusal() { self.detail_redacted() } else { None }
    }

    /// `true` when the attempt was refused locally instead of failing upstream.
    #[must_use]
    pub const fn is_refusal(&self) -> bool {
        matches!(
            self.kind,
            ProviderFailureKind::AdmissionRefused | ProviderFailureKind::AllowanceExhausted
        )
    }
}

impl From<&ProviderCallError> for ProviderFailure {
    fn from(error: &ProviderCallError) -> Self {
        let message = error.to_string();
        let detail_redacted = RedactedFailureDetail::sanitized(&error.redacted_detail());
        let kind = if error.is_quota_exhausted() {
            ProviderFailureKind::QuotaExhausted
        } else if let Some(admission) = error.admission_error() {
            // A local admission outcome is never a provider outage: keep the
            // typed kind and the value-free refusal token.
            admission_failure_kind(admission)
        } else if error.is_permanent_rejection() {
            ProviderFailureKind::RequestRejected
        } else {
            ProviderFailureKind::Unavailable
        };
        Self { kind, message, detail_redacted }
    }
}

impl From<ProviderCallError> for ProviderFailure {
    fn from(error: ProviderCallError) -> Self {
        Self::from(&error)
    }
}

/// Classifies a local admission outcome. The refusal subreason stays in the
/// value-free detail token; only the operational capacity variants remain
/// [`ProviderFailureKind::Unavailable`], because they are local, temporary and
/// never a statement about the provider or the financial scope.
const fn admission_failure_kind(error: &ProviderBudgetError) -> ProviderFailureKind {
    match error {
        ProviderBudgetError::Dispatch(ProviderDispatchRefusal::Exhausted) => {
            ProviderFailureKind::AllowanceExhausted
        }
        ProviderBudgetError::Dispatch(_) | ProviderBudgetError::InvalidConfiguration(_) => {
            ProviderFailureKind::AdmissionRefused
        }
        ProviderBudgetError::AcquireTimeout { .. }
        | ProviderBudgetError::ThrottleTimeout { .. }
        | ProviderBudgetError::LimiterClosed
        | ProviderBudgetError::RegistryCapacity { .. }
        | ProviderBudgetError::RegistryPoisoned => ProviderFailureKind::Unavailable,
    }
}

/// Classifies one dispatch refusal on its own, for callers that hold the
/// refusal but not a [`ProviderCallError`]. Keeps the mapping in one place.
#[must_use]
pub const fn refusal_failure_kind(refusal: ProviderDispatchRefusal) -> ProviderFailureKind {
    match refusal {
        ProviderDispatchRefusal::Exhausted => ProviderFailureKind::AllowanceExhausted,
        ProviderDispatchRefusal::Authority
        | ProviderDispatchRefusal::RouteMismatch
        | ProviderDispatchRefusal::CredentialMismatch
        | ProviderDispatchRefusal::Caller
        | ProviderDispatchRefusal::Attribution
        | ProviderDispatchRefusal::Busy
        | ProviderDispatchRefusal::UninspectableBody
        | ProviderDispatchRefusal::Envelope
        | ProviderDispatchRefusal::Admission => ProviderFailureKind::AdmissionRefused,
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;

    use super::*;
    use crate::integrations::retry::provider_http_status_error;

    fn admission_failure(refusal: ProviderDispatchRefusal) -> ProviderFailure {
        let error = ProviderCallError::admission(ProviderBudgetError::Dispatch(refusal));
        ProviderFailure::from(&error)
    }

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
        assert_eq!(failure.refusal_code(), None);
    }

    #[test]
    fn admission_refusals_keep_a_typed_kind_and_value_free_code() {
        for (refusal, expected_kind, expected_code) in [
            (
                ProviderDispatchRefusal::Authority,
                ProviderFailureKind::AdmissionRefused,
                "dispatch_authority",
            ),
            (
                ProviderDispatchRefusal::RouteMismatch,
                ProviderFailureKind::AdmissionRefused,
                "dispatch_route_mismatch",
            ),
            (
                ProviderDispatchRefusal::CredentialMismatch,
                ProviderFailureKind::AdmissionRefused,
                "dispatch_credential_mismatch",
            ),
            (
                ProviderDispatchRefusal::UninspectableBody,
                ProviderFailureKind::AdmissionRefused,
                "dispatch_uninspectable_body",
            ),
            (
                ProviderDispatchRefusal::Envelope,
                ProviderFailureKind::AdmissionRefused,
                "dispatch_envelope",
            ),
            (
                ProviderDispatchRefusal::Admission,
                ProviderFailureKind::AdmissionRefused,
                "dispatch_admission",
            ),
            (
                ProviderDispatchRefusal::Exhausted,
                ProviderFailureKind::AllowanceExhausted,
                "dispatch_exhausted",
            ),
        ] {
            let failure = admission_failure(refusal.clone());
            assert_eq!(failure.kind(), expected_kind, "{refusal:?}");
            assert_eq!(expected_kind, refusal_failure_kind(refusal.clone()));
            assert!(failure.is_refusal(), "{refusal:?}");
            assert_eq!(failure.refusal_code(), Some(expected_code), "{refusal:?}");
        }
    }

    #[test]
    fn exhaustion_is_not_reported_as_an_ordinary_admission_refusal() {
        let exhausted = admission_failure(ProviderDispatchRefusal::Exhausted);
        let authority = admission_failure(ProviderDispatchRefusal::Authority);

        assert_ne!(exhausted.kind(), authority.kind());
        assert_eq!(exhausted.kind_label(), "allowance exhausted");
        assert_eq!(authority.kind_label(), "admission refused");
    }

    #[test]
    fn local_capacity_failures_stay_operational_and_carry_no_refusal_code() {
        let failure = ProviderFailure::from(&ProviderCallError::admission(
            ProviderBudgetError::ThrottleTimeout { timeout_ms: 25 },
        ));

        assert_eq!(failure.kind(), ProviderFailureKind::Unavailable);
        assert_eq!(failure.refusal_code(), None);
        // The value-free diagnostic token still records the local cause.
        assert_eq!(failure.detail_redacted(), Some("admission_throttle_timeout"));
    }

    #[test]
    fn prose_constructors_carry_no_invented_detail() {
        let failure = ProviderFailure::unavailable("agent loop stopped on provider error");
        assert_eq!(failure.kind(), ProviderFailureKind::Unavailable);
        assert_eq!(failure.detail_redacted(), None);
        assert_eq!(failure.to_string(), "agent loop stopped on provider error");

        let refused = ProviderFailure::admission_refused("dispatch refused");
        assert_eq!(refused.kind(), ProviderFailureKind::AdmissionRefused);
        assert_eq!(refused.refusal_code(), None);

        let exhausted = ProviderFailure::allowance_exhausted("allowance exhausted");
        assert_eq!(exhausted.kind(), ProviderFailureKind::AllowanceExhausted);
        assert_eq!(exhausted.refusal_code(), None);
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
