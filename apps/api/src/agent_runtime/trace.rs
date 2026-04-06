use crate::domains::agent_runtime::{
    RuntimeActionRecord, RuntimeDecisionKind, RuntimeExecution, RuntimePolicyDecision,
    RuntimePolicyDecisionSummary, RuntimePolicySummary, RuntimeStageRecord,
};

const MAX_RUNTIME_POLICY_SUMMARY_DECISIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStageTraceSummary {
    pub stage_kind: crate::domains::agent_runtime::RuntimeStageKind,
    pub stage_state: crate::domains::agent_runtime::RuntimeStageState,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeActionTraceSummary {
    pub action_kind: crate::domains::agent_runtime::RuntimeActionKind,
    pub action_state: crate::domains::agent_runtime::RuntimeActionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeExecutionTraceView {
    pub execution: RuntimeExecution,
    pub stages: Vec<RuntimeStageRecord>,
    pub actions: Vec<RuntimeActionRecord>,
    pub policy_decisions: Vec<RuntimePolicyDecision>,
}

impl RuntimeExecutionTraceView {
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(execution: RuntimeExecution) -> Self {
        Self { execution, stages: Vec::new(), actions: Vec::new(), policy_decisions: Vec::new() }
    }
}

#[must_use]
#[allow(clippy::missing_const_for_fn)]
pub fn build_trace_view(
    execution: RuntimeExecution,
    stages: Vec<RuntimeStageRecord>,
    actions: Vec<RuntimeActionRecord>,
    policy_decisions: Vec<RuntimePolicyDecision>,
) -> RuntimeExecutionTraceView {
    RuntimeExecutionTraceView { execution, stages, actions, policy_decisions }
}

#[must_use]
pub fn stage_summaries(trace: &RuntimeExecutionTraceView) -> Vec<RuntimeStageTraceSummary> {
    trace
        .stages
        .iter()
        .map(|record| RuntimeStageTraceSummary {
            stage_kind: record.stage_kind,
            stage_state: record.stage_state,
            failure_code: record.failure_code.clone(),
        })
        .collect()
}

#[must_use]
pub fn action_summaries(trace: &RuntimeExecutionTraceView) -> Vec<RuntimeActionTraceSummary> {
    trace
        .actions
        .iter()
        .map(|record| RuntimeActionTraceSummary {
            action_kind: record.action_kind,
            action_state: record.action_state,
        })
        .collect()
}

#[must_use]
pub fn redacted_failure_summary(trace: &RuntimeExecutionTraceView) -> Option<String> {
    trace
        .execution
        .failure_summary_redacted
        .clone()
        .or_else(|| trace.stages.iter().find_map(|record| record.failure_summary_redacted.clone()))
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
