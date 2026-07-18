use serde::{Deserialize, Serialize};

use crate::domains::agent_runtime::{
    RuntimeDecisionKind, RuntimePolicyDecisionSummary, RuntimePolicySummary,
};

const MAX_RUNTIME_FAILURE_PUBLIC_SUMMARY_CHARS: usize = 120;

/// Returns a public-safe summary for a canonical typed failure code.
///
/// Arbitrary diagnostics are intentionally not accepted. Failure codes are
/// protocol identifiers and must use bounded ASCII `snake_case`.
#[must_use]
pub(crate) fn canonical_runtime_failure_summary(code: &str) -> Option<String> {
    let bytes = code.as_bytes();
    let first = bytes.first().copied()?;
    let last = bytes.last().copied()?;
    let is_canonical = bytes.len() <= MAX_RUNTIME_FAILURE_PUBLIC_SUMMARY_CHARS
        && first.is_ascii_lowercase()
        && last.is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
    is_canonical.then(|| code.to_string())
}

/// Identifies protocol-level failures whose detail originates at the typed
/// runtime-policy boundary and is already redacted.
#[must_use]
pub(crate) fn is_runtime_policy_failure_code(code: &str) -> bool {
    matches!(
        code,
        "runtime_policy_rejected" | "runtime_policy_terminated" | "runtime_policy_blocked"
    )
}

/// Bounds a summary whose policy-domain type already guarantees redaction.
/// This must never be used for provider, repository, document, or user text.
#[must_use]
pub(crate) fn bounded_redacted_runtime_policy_summary(summary_redacted: &str) -> Option<String> {
    let summary_redacted = summary_redacted.trim();
    if summary_redacted.is_empty() {
        return None;
    }
    Some(summary_redacted.chars().take(MAX_RUNTIME_FAILURE_PUBLIC_SUMMARY_CHARS).collect())
}

/// Projects a durable runtime failure into a public-safe view.
///
/// The stored `failure_summary_redacted` column is deliberately not an input:
/// old rows may contain truncate-only provider or user diagnostics. A public
/// view may expose only the canonical failure code or a summary carried by a
/// matching typed runtime-policy decision.
#[must_use]
pub(crate) fn public_runtime_failure_summary(
    failure_code: Option<&str>,
    policy_summary: &RuntimePolicySummary,
) -> Option<String> {
    let failure_code = failure_code?;
    policy_summary
        .recent_decisions
        .iter()
        .rev()
        .find(|decision| policy_decision_owns_failure(decision, failure_code))
        .and_then(|decision| {
            bounded_redacted_runtime_policy_summary(&decision.reason_summary_redacted)
        })
        .or_else(|| canonical_runtime_failure_summary(failure_code))
}

fn policy_decision_owns_failure(
    decision: &RuntimePolicyDecisionSummary,
    failure_code: &str,
) -> bool {
    let is_terminal = matches!(
        decision.decision_kind,
        RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate
    );
    is_terminal
        && (decision.reason_code == failure_code || is_runtime_policy_failure_code(failure_code))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRecoveryOutcome {
    pub attempts: u8,
    pub summary_redacted: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFailureSummary {
    pub code: String,
    pub summary_redacted: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTerminalOutcome<TSuccess, TFailure> {
    Completed { success: TSuccess },
    Recovered { success: TSuccess, recovery: RuntimeRecoveryOutcome },
    Failed { failure: TFailure, summary: RuntimeFailureSummary },
    Canceled { failure: TFailure, summary: RuntimeFailureSummary },
}

#[cfg(test)]
mod tests {
    use super::{canonical_runtime_failure_summary, public_runtime_failure_summary};
    use crate::domains::agent_runtime::{
        RuntimeDecisionKind, RuntimeDecisionTargetKind, RuntimePolicyDecisionSummary,
        RuntimePolicySummary,
    };

    #[test]
    fn canonical_runtime_failure_summary_rejects_diagnostic_text() {
        let private_diagnostic = "provider failure: runtime-code-sentinel-secret";

        assert!(canonical_runtime_failure_summary(private_diagnostic).is_none());
    }

    #[test]
    fn public_runtime_failure_summary_accepts_typed_redacted_policy_detail() {
        let redacted_policy_detail = "operator-approved redacted policy detail";
        let policy_summary = RuntimePolicySummary {
            reject_count: 1,
            recent_decisions: vec![RuntimePolicyDecisionSummary {
                target_kind: RuntimeDecisionTargetKind::FinalOutcome,
                decision_kind: RuntimeDecisionKind::Reject,
                reason_code: "operator_policy".to_string(),
                reason_summary_redacted: redacted_policy_detail.to_string(),
            }],
            ..RuntimePolicySummary::default()
        };

        assert_eq!(
            public_runtime_failure_summary(Some("runtime_policy_rejected"), &policy_summary)
                .as_deref(),
            Some(redacted_policy_detail)
        );
    }
}
