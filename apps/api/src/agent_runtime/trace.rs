use crate::domains::agent_runtime::{
    RuntimeActionRecord, RuntimeDecisionKind, RuntimeExecution, RuntimePolicyDecision,
    RuntimePolicyDecisionSummary, RuntimePolicySummary, RuntimeStageRecord,
};

const MAX_RUNTIME_POLICY_SUMMARY_DECISIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeExecutionTraceView {
    pub execution: RuntimeExecution,
    pub stages: Vec<RuntimeStageRecord>,
    pub actions: Vec<RuntimeActionRecord>,
    pub policy_decisions: Vec<RuntimePolicyDecision>,
}

impl RuntimeExecutionTraceView {
    #[must_use]
    pub const fn new(execution: RuntimeExecution) -> Self {
        Self { execution, stages: Vec::new(), actions: Vec::new(), policy_decisions: Vec::new() }
    }
}

#[must_use]
pub const fn build_trace_view(
    execution: RuntimeExecution,
    stages: Vec<RuntimeStageRecord>,
    actions: Vec<RuntimeActionRecord>,
    policy_decisions: Vec<RuntimePolicyDecision>,
) -> RuntimeExecutionTraceView {
    RuntimeExecutionTraceView { execution, stages, actions, policy_decisions }
}

#[must_use]
pub fn build_policy_summary(policy_decisions: &[RuntimePolicyDecision]) -> RuntimePolicySummary {
    let mut summary = RuntimePolicySummary::default();
    for decision in policy_decisions {
        match decision.decision_kind {
            RuntimeDecisionKind::Allow => summary.allow_count += 1,
            RuntimeDecisionKind::Reject => summary.reject_count += 1,
            RuntimeDecisionKind::Terminate => summary.terminate_count += 1,
        }
    }
    summary.recent_decisions = policy_decisions
        .iter()
        .rev()
        .take(MAX_RUNTIME_POLICY_SUMMARY_DECISIONS)
        .map(|decision| RuntimePolicyDecisionSummary {
            target_kind: decision.target_kind,
            decision_kind: decision.decision_kind,
            reason_code: decision.reason_code.clone(),
            reason_summary_redacted: decision.reason_summary_redacted.clone(),
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    summary
}

#[must_use]
pub fn policy_summary(trace: &RuntimeExecutionTraceView) -> RuntimePolicySummary {
    build_policy_summary(&trace.policy_decisions)
}
