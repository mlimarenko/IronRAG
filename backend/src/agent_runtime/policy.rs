use async_trait::async_trait;

use crate::domains::agent_runtime::{
    RuntimeDecisionKind, RuntimeDecisionTargetKind, RuntimeTaskKind,
};

pub const DEFAULT_RUNTIME_POLICY_REASON_CODE: &str = "runtime_policy";
pub const DEFAULT_RUNTIME_POLICY_REASON_SUMMARY: &str = "runtime policy decision";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicyContext {
    pub execution_id: uuid::Uuid,
    pub task_kind: RuntimeTaskKind,
    pub target_kind: RuntimeDecisionTargetKind,
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePolicyOutcome {
    pub decision_kind: RuntimeDecisionKind,
    pub reason_code: Option<String>,
    pub reason_summary_redacted: Option<String>,
}

impl RuntimePolicyOutcome {
    #[must_use]
    pub const fn allow() -> Self {
        Self {
            decision_kind: RuntimeDecisionKind::Allow,
            reason_code: None,
            reason_summary_redacted: None,
        }
    }

    #[must_use]
    pub fn reject(
        reason_code: impl Into<String>,
        reason_summary_redacted: impl Into<String>,
    ) -> Self {
        Self {
            decision_kind: RuntimeDecisionKind::Reject,
            reason_code: Some(reason_code.into()),
            reason_summary_redacted: Some(reason_summary_redacted.into()),
        }
    }

    #[must_use]
    pub fn terminate(
        reason_code: impl Into<String>,
        reason_summary_redacted: impl Into<String>,
    ) -> Self {
        Self {
            decision_kind: RuntimeDecisionKind::Terminate,
            reason_code: Some(reason_code.into()),
            reason_summary_redacted: Some(reason_summary_redacted.into()),
        }
    }

    #[must_use]
    pub fn resolved_reason_code(&self) -> String {
        self.reason_code.clone().unwrap_or_else(|| DEFAULT_RUNTIME_POLICY_REASON_CODE.to_string())
    }

    #[must_use]
    pub fn resolved_reason_summary(&self, fallback: &'static str) -> String {
        self.reason_summary_redacted.clone().unwrap_or_else(|| fallback.to_string())
    }
}

#[async_trait]
pub trait RuntimePolicy: Send + Sync {
    async fn evaluate(&self, context: &RuntimePolicyContext) -> RuntimePolicyOutcome;
}
