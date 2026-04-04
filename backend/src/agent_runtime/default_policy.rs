use std::collections::BTreeSet;

use async_trait::async_trait;

use crate::{
    agent_runtime::policy::{RuntimePolicy, RuntimePolicyContext, RuntimePolicyOutcome},
    domains::agent_runtime::{RuntimeDecisionKind, RuntimeDecisionTargetKind, RuntimeTaskKind},
};

#[derive(Clone, Debug, Default)]
pub struct DefaultRuntimePolicyRules {
    rejected_task_kinds: BTreeSet<RuntimeTaskKind>,
    rejected_target_kinds: BTreeSet<RuntimeDecisionTargetKind>,
}

impl DefaultRuntimePolicyRules {
    #[must_use]
    pub fn new(
        rejected_task_kinds: impl IntoIterator<Item = RuntimeTaskKind>,
        rejected_target_kinds: impl IntoIterator<Item = RuntimeDecisionTargetKind>,
    ) -> Self {
        Self {
            rejected_task_kinds: rejected_task_kinds.into_iter().collect(),
            rejected_target_kinds: rejected_target_kinds.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn should_reject(
        &self,
        task_kind: RuntimeTaskKind,
        target_kind: RuntimeDecisionTargetKind,
    ) -> bool {
        !self.rejected_task_kinds.is_empty()
            && self.rejected_task_kinds.contains(&task_kind)
            && (self.rejected_target_kinds.is_empty()
                || self.rejected_target_kinds.contains(&target_kind))
    }
}

#[derive(Clone, Debug)]
pub struct DefaultRuntimePolicy {
    reason_budget_chars: usize,
    rules: DefaultRuntimePolicyRules,
}

impl DefaultRuntimePolicy {
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(reason_budget_chars: usize, rules: DefaultRuntimePolicyRules) -> Self {
        Self { reason_budget_chars, rules }
    }

    fn bounded_summary(&self, summary: &'static str) -> String {
        if summary.chars().count() <= self.reason_budget_chars {
            return summary.to_string();
        }
        summary.chars().take(self.reason_budget_chars).collect()
    }

    fn allow_for_target(&self, target_kind: RuntimeDecisionTargetKind) -> RuntimePolicyOutcome {
        let (reason_code, reason_summary) = match target_kind {
            RuntimeDecisionTargetKind::ModelRequest => (
                "default_policy_allow_model_request",
                "Default runtime policy allowed the model request.",
            ),
            RuntimeDecisionTargetKind::ToolRequest => (
                "default_policy_allow_tool_request",
                "Default runtime policy allowed the tool request.",
            ),
            RuntimeDecisionTargetKind::ToolResult => (
                "default_policy_allow_tool_result",
                "Default runtime policy allowed the tool result.",
            ),
            RuntimeDecisionTargetKind::StageTransition => (
                "default_policy_allow_stage_transition",
                "Default runtime policy allowed the stage transition.",
            ),
            RuntimeDecisionTargetKind::FinalOutcome => (
                "default_policy_allow_final_outcome",
                "Default runtime policy allowed the final outcome.",
            ),
        };

        RuntimePolicyOutcome {
            decision_kind: RuntimeDecisionKind::Allow,
            reason_code: Some(reason_code.to_string()),
            reason_summary_redacted: Some(self.bounded_summary(reason_summary)),
        }
    }

    fn reject_for_target(
        &self,
        task_kind: RuntimeTaskKind,
        target_kind: RuntimeDecisionTargetKind,
    ) -> RuntimePolicyOutcome {
        let (reason_code, reason_summary) = match target_kind {
            RuntimeDecisionTargetKind::ModelRequest => {
                ("runtime_policy_rejected", "Default runtime policy rejected the model request.")
            }
            RuntimeDecisionTargetKind::ToolRequest => {
                ("runtime_policy_rejected", "Default runtime policy rejected the tool request.")
            }
            RuntimeDecisionTargetKind::ToolResult => {
                ("runtime_policy_rejected", "Default runtime policy rejected the tool result.")
            }
            RuntimeDecisionTargetKind::StageTransition => {
                ("runtime_policy_rejected", "Default runtime policy rejected the stage transition.")
            }
            RuntimeDecisionTargetKind::FinalOutcome => {
                ("runtime_policy_rejected", "Default runtime policy rejected the final outcome.")
            }
        };
        RuntimePolicyOutcome::reject(
            reason_code,
            format!(
                "{} Task kind: {}. Target kind: {}.",
                self.bounded_summary(reason_summary),
                task_kind.as_str(),
                target_kind.as_str()
            ),
        )
    }
}

#[async_trait]
impl RuntimePolicy for DefaultRuntimePolicy {
    async fn evaluate(&self, context: &RuntimePolicyContext) -> RuntimePolicyOutcome {
        if self.rules.should_reject(context.task_kind, context.target_kind) {
            return self.reject_for_target(context.task_kind, context.target_kind);
        }
        self.allow_for_target(context.target_kind)
    }
}
