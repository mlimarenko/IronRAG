use chrono::{DateTime, Utc};
use serde_json::json;
use std::{future::Future, sync::Arc};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    agent_runtime::{
        hooks::{RuntimeHookContext, RuntimeHooks},
        policy::{RuntimePolicy, RuntimePolicyContext},
        registry::RuntimeTaskRegistry,
        response::{RuntimeFailureSummary, RuntimeTerminalOutcome},
        task::{RuntimeTask, RuntimeTaskRequest, RuntimeTaskResult},
        trace::RuntimeExecutionTraceView,
    },
    domains::agent_runtime::{
        RuntimeActionKind, RuntimeActionRecord, RuntimeActionState, RuntimeDecisionKind,
        RuntimeDecisionTargetKind, RuntimeExecution, RuntimeLifecycleState, RuntimePolicyDecision,
        RuntimeStageKind, RuntimeStageRecord, RuntimeStageState, RuntimeTaskKind,
    },
};

#[derive(thiserror::Error, Debug)]
pub enum RuntimeExecutionError {
    #[error("{0}")]
    InvalidTaskSpec(String),
    #[error("runtime task is not registered: {0:?}")]
    UnregisteredTask(crate::domains::agent_runtime::RuntimeTaskKind),
    #[error("runtime execution budget exhausted")]
    TurnBudgetExhausted,
    #[error("runtime policy blocked execution: {reason_code}")]
    PolicyBlocked {
        decision_kind: RuntimeDecisionKind,
        reason_code: String,
        reason_summary_redacted: String,
    },
}

pub struct RuntimeExecutionSession {
    pub execution: RuntimeExecution,
    pub trace: RuntimeExecutionTraceView,
    action_semaphore: Arc<Semaphore>,
    remaining_turns: u8,
}

/// A policy decision and the optional stage/action rows it belongs to.
struct PolicyDecisionEntry {
    target_kind: RuntimeDecisionTargetKind,
    decision_kind: RuntimeDecisionKind,
    reason_code: Option<String>,
    reason_summary_redacted: Option<String>,
    stage_record_id: Option<Uuid>,
    action_record_id: Option<Uuid>,
}

/// Scope and reason presented to policy and hooks for one runtime target.
struct TargetEvaluation {
    execution_id: Uuid,
    task_kind: RuntimeTaskKind,
    stage_kind: Option<RuntimeStageKind>,
    action_kind: Option<RuntimeActionKind>,
    target_kind: RuntimeDecisionTargetKind,
    reason_code: Option<String>,
}

/// A completed stage row to append to a session trace. `started_at` is the
/// wall-clock moment captured when the stage entered (returned by
/// [`RuntimeExecutor::begin_stage`]); `completed_at` is stamped when the row is
/// written.
pub struct StageCompletion {
    pub stage_kind: RuntimeStageKind,
    pub stage_state: RuntimeStageState,
    pub deterministic: bool,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
    pub started_at: DateTime<Utc>,
}

impl RuntimeExecutionSession {
    #[must_use]
    pub const fn remaining_turns(&self) -> u8 {
        self.remaining_turns
    }

    #[must_use]
    pub fn action_semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.action_semaphore)
    }

    /// # Errors
    /// Returns [`RuntimeExecutionError::TurnBudgetExhausted`] when no model or tool turns remain.
    pub const fn consume_turn(&mut self) -> Result<(), RuntimeExecutionError> {
        if self.remaining_turns == 0 {
            return Err(RuntimeExecutionError::TurnBudgetExhausted);
        }
        self.remaining_turns -= 1;
        self.execution.turn_count += 1;
        Ok(())
    }

    /// Record a trace row for a completed stage. `started_at` is
    /// captured by the caller when the stage *begins* (returned by
    /// [`RuntimeExecutor::begin_stage`]) and threaded through to
    /// this call; `completed_at` is stamped here, at the moment the
    /// trace row is actually written. The previous behaviour stamped
    /// both timestamps with the same `Utc::now()` — the runtime
    /// trace viewer consequently showed every stage as taking ~0 ms,
    /// which made per-stage perf investigation impossible.
    pub fn record_stage(
        &mut self,
        stage_kind: RuntimeStageKind,
        stage_state: RuntimeStageState,
        deterministic: bool,
        started_at: DateTime<Utc>,
    ) -> Uuid {
        let record_id = Uuid::now_v7();
        self.trace.stages.push(RuntimeStageRecord {
            id: record_id,
            runtime_execution_id: self.execution.id,
            stage_kind,
            stage_ordinal: i32::try_from(self.trace.stages.len() + 1).unwrap_or(i32::MAX),
            attempt_no: 1,
            stage_state,
            deterministic,
            started_at,
            completed_at: Some(Utc::now()),
            input_summary_json: json!({}),
            output_summary_json: json!({}),
            failure_code: None,
            failure_summary_redacted: None,
        });
        self.execution.active_stage =
            if matches!(stage_state, RuntimeStageState::Running) { Some(stage_kind) } else { None };
        record_id
    }

    pub fn record_action(
        &mut self,
        stage_record_id: Uuid,
        action_kind: RuntimeActionKind,
        action_state: RuntimeActionState,
    ) -> Uuid {
        let action_id = Uuid::now_v7();
        self.trace.actions.push(RuntimeActionRecord {
            id: action_id,
            runtime_execution_id: self.execution.id,
            stage_record_id,
            action_kind,
            action_ordinal: i32::try_from(self.trace.actions.len() + 1).unwrap_or(i32::MAX),
            action_state,
            provider_binding_id: None,
            tool_name: None,
            usage_json: None,
            summary_json: json!({}),
            created_at: Utc::now(),
        });
        action_id
    }

    fn record_policy_decision(&mut self, entry: PolicyDecisionEntry) {
        self.trace.policy_decisions.push(build_policy_decision(self.execution.id, entry));
    }
}

fn build_policy_decision(execution_id: Uuid, entry: PolicyDecisionEntry) -> RuntimePolicyDecision {
    RuntimePolicyDecision {
        id: Uuid::now_v7(),
        runtime_execution_id: execution_id,
        stage_record_id: entry.stage_record_id,
        action_record_id: entry.action_record_id,
        target_kind: entry.target_kind,
        decision_kind: entry.decision_kind,
        reason_code: entry.reason_code.unwrap_or_else(|| "runtime_policy".to_string()),
        reason_summary_redacted: entry
            .reason_summary_redacted
            .unwrap_or_else(|| "runtime policy decision".to_string()),
        created_at: Utc::now(),
    }
}

const fn map_action_target_kind(
    action_kind: RuntimeActionKind,
) -> Option<RuntimeDecisionTargetKind> {
    match action_kind {
        RuntimeActionKind::ModelRequest => Some(RuntimeDecisionTargetKind::ModelRequest),
        RuntimeActionKind::ToolRequest => Some(RuntimeDecisionTargetKind::ToolRequest),
        RuntimeActionKind::ToolResult => Some(RuntimeDecisionTargetKind::ToolResult),
        RuntimeActionKind::DeterministicStep
        | RuntimeActionKind::RecoveryAttempt
        | RuntimeActionKind::PersistenceWrite => None,
    }
}

#[derive(Clone)]
pub struct RuntimeExecutor {
    registry: RuntimeTaskRegistry,
    policy: Arc<dyn RuntimePolicy>,
    hooks: Arc<dyn RuntimeHooks>,
}

impl RuntimeExecutor {
    #[must_use]
    pub fn new(
        registry: RuntimeTaskRegistry,
        policy: Arc<dyn RuntimePolicy>,
        hooks: Arc<dyn RuntimeHooks>,
    ) -> Self {
        Self { registry, policy, hooks }
    }

    async fn run_before_hooks(&self, context: &RuntimeHookContext) {
        self.hooks.before_target(context).await;
        match context.target_kind {
            RuntimeDecisionTargetKind::ModelRequest => {
                self.hooks.before_model_request(context).await;
            }
            RuntimeDecisionTargetKind::ToolRequest => self.hooks.before_tool_request(context).await,
            RuntimeDecisionTargetKind::ToolResult => self.hooks.before_tool_result(context).await,
            RuntimeDecisionTargetKind::StageTransition => {
                self.hooks.before_stage_transition(context).await;
            }
            RuntimeDecisionTargetKind::FinalOutcome => {
                self.hooks.before_final_outcome(context).await;
            }
        }
    }

    async fn run_after_hooks(&self, context: &RuntimeHookContext) {
        match context.target_kind {
            RuntimeDecisionTargetKind::ModelRequest => {
                self.hooks.after_model_request(context).await;
            }
            RuntimeDecisionTargetKind::ToolRequest => self.hooks.after_tool_request(context).await,
            RuntimeDecisionTargetKind::ToolResult => self.hooks.after_tool_result(context).await,
            RuntimeDecisionTargetKind::StageTransition => {
                self.hooks.after_stage_transition(context).await;
            }
            RuntimeDecisionTargetKind::FinalOutcome => {
                self.hooks.after_final_outcome(context).await;
            }
        }
        self.hooks.after_target(context).await;
    }

    async fn evaluate_target(
        &self,
        evaluation: TargetEvaluation,
    ) -> crate::agent_runtime::policy::RuntimePolicyOutcome {
        let TargetEvaluation {
            execution_id,
            task_kind,
            stage_kind,
            action_kind,
            target_kind,
            reason_code,
        } = evaluation;
        let hook_context =
            RuntimeHookContext { execution_id, stage_kind, action_kind, target_kind };
        self.run_before_hooks(&hook_context).await;
        let decision = self
            .policy
            .evaluate(&RuntimePolicyContext { execution_id, task_kind, target_kind, reason_code })
            .await;
        self.run_after_hooks(&hook_context).await;
        decision
    }

    /// Mark a stage as entered and return the wall-clock moment the
    /// stage actually started. The caller holds on to this timestamp
    /// and passes it back to [`Self::complete_stage`] so the trace row
    /// records real elapsed time instead of the old `started_at == completed_at`
    /// zero.
    ///
    /// # Errors
    /// Returns [`RuntimeExecutionError::PolicyBlocked`] when runtime policy rejects the stage.
    pub async fn begin_stage(
        &self,
        session: &mut RuntimeExecutionSession,
        stage_kind: RuntimeStageKind,
    ) -> Result<DateTime<Utc>, RuntimeExecutionError> {
        if session.execution.lifecycle_state.is_terminal() {
            let reason_code = session
                .execution
                .failure_code
                .clone()
                .unwrap_or_else(|| "runtime_execution_already_terminal".to_string());
            let reason_summary_redacted = session
                .execution
                .failure_summary_redacted
                .clone()
                .unwrap_or_else(|| "runtime execution is already terminal".to_string());
            return Err(RuntimeExecutionError::PolicyBlocked {
                decision_kind: RuntimeDecisionKind::Terminate,
                reason_code,
                reason_summary_redacted,
            });
        }
        // Capture the real stage-entry timestamp BEFORE the policy
        // round-trip — evaluate_target can take several ms on some
        // policies, and attributing that setup cost to the stage
        // itself is exactly the kind of fudge the old "zero-diff"
        // trace row was hiding.
        let started_at = Utc::now();
        let decision = self
            .evaluate_target(TargetEvaluation {
                execution_id: session.execution.id,
                task_kind: session.execution.task_kind,
                stage_kind: Some(stage_kind),
                action_kind: None,
                target_kind: RuntimeDecisionTargetKind::StageTransition,
                reason_code: Some(format!("stage_transition:{}", stage_kind.as_str())),
            })
            .await;
        session.record_policy_decision(PolicyDecisionEntry {
            target_kind: RuntimeDecisionTargetKind::StageTransition,
            decision_kind: decision.decision_kind,
            reason_code: decision.reason_code.clone(),
            reason_summary_redacted: decision.reason_summary_redacted.clone(),
            stage_record_id: None,
            action_record_id: None,
        });
        match decision.decision_kind {
            RuntimeDecisionKind::Allow => {
                session.execution.lifecycle_state = RuntimeLifecycleState::Running;
                session.execution.active_stage = Some(stage_kind);
                Ok(started_at)
            }
            RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate => {
                let reason_code = decision.resolved_reason_code();
                let reason_summary_redacted =
                    decision.resolved_reason_summary("runtime policy blocked stage transition");
                session.execution.failure_code = Some(reason_code.clone());
                session.execution.failure_summary_redacted = Some(reason_summary_redacted.clone());
                session.execution.lifecycle_state = RuntimeLifecycleState::Canceled;
                session.execution.active_stage = None;
                session.execution.completed_at = Some(Utc::now());
                Err(RuntimeExecutionError::PolicyBlocked {
                    decision_kind: decision.decision_kind,
                    reason_code,
                    reason_summary_redacted,
                })
            }
        }
    }

    pub fn complete_stage(
        &self,
        session: &mut RuntimeExecutionSession,
        completion: StageCompletion,
    ) -> Uuid {
        let StageCompletion {
            stage_kind,
            stage_state,
            deterministic,
            failure_code,
            failure_summary_redacted,
            started_at,
        } = completion;
        let stage_id = session.record_stage(stage_kind, stage_state, deterministic, started_at);
        if let Some(record) = session.trace.stages.iter_mut().find(|record| record.id == stage_id) {
            record.failure_code = failure_code;
            record.failure_summary_redacted = failure_summary_redacted;
        }
        stage_id
    }

    /// # Errors
    /// Returns [`RuntimeExecutionError::TurnBudgetExhausted`] when the action would exceed the
    /// runtime turn budget, or [`RuntimeExecutionError::PolicyBlocked`] when policy rejects it.
    pub async fn begin_action(
        &self,
        session: &mut RuntimeExecutionSession,
        stage_record_id: Uuid,
        stage_kind: RuntimeStageKind,
        action_kind: RuntimeActionKind,
    ) -> Result<Uuid, RuntimeExecutionError> {
        if matches!(action_kind, RuntimeActionKind::ModelRequest | RuntimeActionKind::ToolRequest) {
            session.consume_turn()?;
        }
        if let Some(target_kind) = map_action_target_kind(action_kind) {
            let decision = self
                .evaluate_target(TargetEvaluation {
                    execution_id: session.execution.id,
                    task_kind: session.execution.task_kind,
                    stage_kind: Some(stage_kind),
                    action_kind: Some(action_kind),
                    target_kind,
                    reason_code: Some(format!("action:{}", action_kind.as_str())),
                })
                .await;
            let action_id =
                session.record_action(stage_record_id, action_kind, RuntimeActionState::Running);
            session.record_policy_decision(PolicyDecisionEntry {
                target_kind,
                decision_kind: decision.decision_kind,
                reason_code: decision.reason_code.clone(),
                reason_summary_redacted: decision.reason_summary_redacted.clone(),
                stage_record_id: Some(stage_record_id),
                action_record_id: Some(action_id),
            });
            return match decision.decision_kind {
                RuntimeDecisionKind::Allow => Ok(action_id),
                RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate => {
                    let reason_code = decision.resolved_reason_code();
                    let reason_summary_redacted =
                        decision.resolved_reason_summary("runtime policy blocked action");
                    session.execution.failure_code = Some(reason_code.clone());
                    session.execution.failure_summary_redacted =
                        Some(reason_summary_redacted.clone());
                    session.execution.lifecycle_state = RuntimeLifecycleState::Canceled;
                    session.execution.active_stage = None;
                    session.execution.completed_at = Some(Utc::now());
                    Err(RuntimeExecutionError::PolicyBlocked {
                        decision_kind: decision.decision_kind,
                        reason_code,
                        reason_summary_redacted,
                    })
                }
            };
        }

        Ok(session.record_action(stage_record_id, action_kind, RuntimeActionState::Running))
    }

    pub fn complete_action(
        &self,
        session: &mut RuntimeExecutionSession,
        action_id: Uuid,
        action_state: RuntimeActionState,
    ) {
        if let Some(record) = session.trace.actions.iter_mut().find(|record| record.id == action_id)
        {
            record.action_state = action_state;
        }
    }

    /// # Errors
    /// Returns any validation or registry error that prevents the runtime execution from being
    /// accepted.
    pub async fn prepare<TTask: RuntimeTask>(
        &self,
        request: &RuntimeTaskRequest<TTask>,
    ) -> Result<RuntimeExecution, RuntimeExecutionError> {
        let (execution, _) = self.prepare_with_policy(request).await?;
        Ok(execution)
    }

    async fn prepare_with_policy<TTask: RuntimeTask>(
        &self,
        request: &RuntimeTaskRequest<TTask>,
    ) -> Result<
        (RuntimeExecution, crate::agent_runtime::policy::RuntimePolicyOutcome),
        RuntimeExecutionError,
    > {
        let spec = TTask::spec();
        spec.validate().map_err(RuntimeExecutionError::InvalidTaskSpec)?;
        if !self.registry.contains_task_kind(spec.task_kind) {
            return Err(RuntimeExecutionError::UnregisteredTask(spec.task_kind));
        }

        let execution_id = Uuid::now_v7();
        let decision = self
            .evaluate_target(TargetEvaluation {
                execution_id,
                task_kind: spec.task_kind,
                stage_kind: None,
                action_kind: None,
                target_kind: RuntimeDecisionTargetKind::FinalOutcome,
                reason_code: Some("execution_acceptance".to_string()),
            })
            .await;

        let lifecycle_state = match decision.decision_kind {
            RuntimeDecisionKind::Allow => RuntimeLifecycleState::Accepted,
            RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate => {
                RuntimeLifecycleState::Canceled
            }
        };
        let failure_code = matches!(
            decision.decision_kind,
            RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate
        )
        .then(|| decision.resolved_reason_code());
        let failure_summary_redacted = matches!(
            decision.decision_kind,
            RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate
        )
        .then(|| decision.resolved_reason_summary("runtime policy blocked execution acceptance"));
        let turn_budget = request
            .runtime_overrides
            .and_then(|overrides| overrides.max_turns)
            .unwrap_or(spec.max_turns);
        let parallel_action_limit = request
            .runtime_overrides
            .and_then(|overrides| overrides.max_parallel_actions)
            .unwrap_or(spec.max_parallel_actions);

        Ok((
            RuntimeExecution {
                id: execution_id,
                owner_kind: request.execution_owner.owner_kind,
                owner_id: request.execution_owner.owner_id,
                task_kind: spec.task_kind,
                surface_kind: request.surface_kind_override.unwrap_or(spec.surface_kind),
                contract_name: request.contract_name.to_string(),
                contract_version: request.contract_version.to_string(),
                lifecycle_state,
                active_stage: None,
                turn_budget: i32::from(turn_budget),
                turn_count: 0,
                parallel_action_limit: i32::from(parallel_action_limit),
                failure_code,
                failure_summary_redacted,
                accepted_at: Utc::now(),
                completed_at: lifecycle_state.is_terminal().then(Utc::now),
            },
            decision,
        ))
    }

    /// # Errors
    /// Returns any validation or registry error that prevents the runtime execution from being
    /// seeded.
    pub async fn seed_result<TTask: RuntimeTask>(
        &self,
        request: &RuntimeTaskRequest<TTask>,
    ) -> Result<(RuntimeExecution, RuntimeExecutionTraceView), RuntimeExecutionError> {
        let (execution, decision) = self.prepare_with_policy(request).await?;
        if matches!(
            decision.decision_kind,
            RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate
        ) {
            return Err(RuntimeExecutionError::PolicyBlocked {
                decision_kind: decision.decision_kind,
                reason_code: decision.resolved_reason_code(),
                reason_summary_redacted: decision
                    .resolved_reason_summary("runtime policy blocked execution acceptance"),
            });
        }
        let mut trace = RuntimeExecutionTraceView::new(execution.clone());
        trace.policy_decisions.push(build_policy_decision(
            execution.id,
            PolicyDecisionEntry {
                target_kind: RuntimeDecisionTargetKind::FinalOutcome,
                decision_kind: decision.decision_kind,
                reason_code: decision.reason_code,
                reason_summary_redacted: decision.reason_summary_redacted,
                stage_record_id: None,
                action_record_id: None,
            },
        ));
        Ok((execution, trace))
    }

    /// # Errors
    /// Returns any validation or registry error that prevents the runtime execution from being
    /// seeded.
    pub async fn seed_session<TTask: RuntimeTask>(
        &self,
        request: &RuntimeTaskRequest<TTask>,
    ) -> Result<RuntimeExecutionSession, RuntimeExecutionError> {
        let (execution, trace) = self.seed_result(request).await?;
        let remaining_turns = u8::try_from(execution.turn_budget).unwrap_or(u8::MAX);
        let parallel_action_limit =
            usize::try_from(execution.parallel_action_limit.max(1)).unwrap_or(usize::MAX);
        Ok(RuntimeExecutionSession {
            execution,
            trace,
            action_semaphore: Arc::new(Semaphore::new(parallel_action_limit)),
            remaining_turns,
        })
    }

    fn apply_terminal_outcome<TTask: RuntimeTask>(
        session: &mut RuntimeExecutionSession,
        outcome: &RuntimeTerminalOutcome<TTask::Success, TTask::Failure>,
    ) {
        session.execution.lifecycle_state = match outcome {
            RuntimeTerminalOutcome::Completed { .. } => RuntimeLifecycleState::Completed,
            RuntimeTerminalOutcome::Recovered { .. } => RuntimeLifecycleState::Recovered,
            RuntimeTerminalOutcome::Failed { summary, .. } => {
                session.execution.failure_code = Some(summary.code.clone());
                session.execution.failure_summary_redacted.clone_from(&summary.summary_redacted);
                RuntimeLifecycleState::Failed
            }
            RuntimeTerminalOutcome::Canceled { summary, .. } => {
                session.execution.failure_code = Some(summary.code.clone());
                session.execution.failure_summary_redacted.clone_from(&summary.summary_redacted);
                RuntimeLifecycleState::Canceled
            }
        };
        if session.execution.lifecycle_state.is_terminal() {
            session.execution.completed_at = Some(Utc::now());
            session.execution.active_stage = None;
        }
    }

    fn finish_session<TTask: RuntimeTask>(
        session: RuntimeExecutionSession,
        outcome: RuntimeTerminalOutcome<TTask::Success, TTask::Failure>,
    ) -> RuntimeTaskResult<TTask> {
        RuntimeTaskResult { execution: session.execution, trace: session.trace, outcome }
    }

    pub async fn finalize_session<TTask: RuntimeTask>(
        &self,
        mut session: RuntimeExecutionSession,
        outcome: RuntimeTerminalOutcome<TTask::Success, TTask::Failure>,
    ) -> RuntimeTaskResult<TTask> {
        let decision = self
            .evaluate_target(TargetEvaluation {
                execution_id: session.execution.id,
                task_kind: session.execution.task_kind,
                stage_kind: session.execution.active_stage,
                action_kind: None,
                target_kind: RuntimeDecisionTargetKind::FinalOutcome,
                reason_code: Some(final_outcome_reason_code(&outcome)),
            })
            .await;
        session.record_policy_decision(PolicyDecisionEntry {
            target_kind: RuntimeDecisionTargetKind::FinalOutcome,
            decision_kind: decision.decision_kind,
            reason_code: decision.reason_code.clone(),
            reason_summary_redacted: decision.reason_summary_redacted.clone(),
            stage_record_id: None,
            action_record_id: None,
        });
        match decision.decision_kind {
            RuntimeDecisionKind::Allow => {
                Self::apply_terminal_outcome::<TTask>(&mut session, &outcome);
                Self::finish_session(session, outcome)
            }
            RuntimeDecisionKind::Reject | RuntimeDecisionKind::Terminate => {
                let reason_code = decision.resolved_reason_code();
                let reason_summary_redacted =
                    decision.resolved_reason_summary("runtime policy rejected the final outcome");
                let failure = TTask::policy_failure(&reason_code, &reason_summary_redacted);
                let canceled = RuntimeTerminalOutcome::Canceled {
                    failure,
                    summary: RuntimeFailureSummary {
                        code: reason_code,
                        summary_redacted: Some(reason_summary_redacted),
                    },
                };
                Self::apply_terminal_outcome::<TTask>(&mut session, &canceled);
                Self::finish_session(session, canceled)
            }
        }
    }

    /// # Errors
    /// Returns any validation or registry error that prevents the runtime execution from being
    /// seeded before the handler runs.
    pub async fn execute_with_handler<TTask, THandler, TFut>(
        &self,
        request: &RuntimeTaskRequest<TTask>,
        handler: THandler,
    ) -> Result<RuntimeTaskResult<TTask>, RuntimeExecutionError>
    where
        TTask: RuntimeTask,
        THandler: FnOnce(&mut RuntimeExecutionSession) -> TFut,
        TFut: Future<Output = RuntimeTerminalOutcome<TTask::Success, TTask::Failure>>,
    {
        let mut session = self.seed_session(request).await?;
        let outcome = handler(&mut session).await;
        Ok(self.finalize_session(session, outcome).await)
    }

    /// # Errors
    /// Returns any validation or registry error that prevents the runtime execution from being
    /// seeded before the terminal outcome is recorded.
    pub async fn execute<TTask: RuntimeTask>(
        &self,
        request: &RuntimeTaskRequest<TTask>,
        outcome: RuntimeTerminalOutcome<TTask::Success, TTask::Failure>,
    ) -> Result<RuntimeTaskResult<TTask>, RuntimeExecutionError> {
        self.execute_with_handler(request, |_session| async move { outcome }).await
    }
}

fn final_outcome_reason_code<TSuccess, TFailure>(
    outcome: &RuntimeTerminalOutcome<TSuccess, TFailure>,
) -> String {
    match outcome {
        RuntimeTerminalOutcome::Completed { .. } => "final_outcome_completed".to_string(),
        RuntimeTerminalOutcome::Recovered { .. } => "final_outcome_recovered".to_string(),
        RuntimeTerminalOutcome::Failed { summary, .. }
        | RuntimeTerminalOutcome::Canceled { summary, .. } => summary.code.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use uuid::Uuid;

    use super::*;
    use crate::{
        agent_runtime::{
            hooks::NoopRuntimeHooks,
            policy::{RuntimePolicy, RuntimePolicyContext, RuntimePolicyOutcome},
            task::RuntimeTaskSpec,
        },
        domains::agent_runtime::{
            RuntimeExecutionOwner, RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeSurfaceKind,
            RuntimeTaskKind,
        },
    };

    struct TestTask;

    impl RuntimeTask for TestTask {
        type Input = String;
        type Success = String;
        type Failure = String;

        const CONTRACT_NAME: &'static str = "executor_test";
        const CONTRACT_VERSION: &'static str = "1";

        fn spec() -> RuntimeTaskSpec {
            RuntimeTaskSpec {
                task_kind: RuntimeTaskKind::GraphExtract,
                surface_kind: RuntimeSurfaceKind::Worker,
                binding_purpose: None,
                machine_consumed: false,
                max_turns: 2,
                max_parallel_actions: 1,
                stage_catalog: &[RuntimeStageKind::ExtractGraph],
                recovery_policy: RuntimeRecoveryPolicy::None,
                output_mode: RuntimeOutputMode::Text,
            }
        }

        fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
            format!("{reason_code}:{reason_summary_redacted}")
        }
    }

    struct AllowPolicy;

    #[async_trait]
    impl RuntimePolicy for AllowPolicy {
        async fn evaluate(&self, _context: &RuntimePolicyContext) -> RuntimePolicyOutcome {
            RuntimePolicyOutcome::allow()
        }
    }

    struct RejectModelRequestPolicy;

    #[async_trait]
    impl RuntimePolicy for RejectModelRequestPolicy {
        async fn evaluate(&self, context: &RuntimePolicyContext) -> RuntimePolicyOutcome {
            if context.target_kind == RuntimeDecisionTargetKind::ModelRequest {
                RuntimePolicyOutcome::reject("model_denied", "model request denied")
            } else {
                RuntimePolicyOutcome::allow()
            }
        }
    }

    struct RejectFinalOutcomePolicy {
        final_outcome_calls: Mutex<u8>,
    }

    #[async_trait]
    impl RuntimePolicy for RejectFinalOutcomePolicy {
        async fn evaluate(&self, context: &RuntimePolicyContext) -> RuntimePolicyOutcome {
            if context.target_kind != RuntimeDecisionTargetKind::FinalOutcome {
                return RuntimePolicyOutcome::allow();
            }
            let mut calls = self.final_outcome_calls.lock().expect("lock final outcome calls");
            *calls += 1;
            if *calls == 1 {
                RuntimePolicyOutcome::allow()
            } else {
                RuntimePolicyOutcome::terminate("outcome_denied", "final outcome denied")
            }
        }
    }

    fn request() -> RuntimeTaskRequest<TestTask> {
        RuntimeTaskRequest::new(
            "test input".to_string(),
            RuntimeExecutionOwner::graph_extraction_attempt(Uuid::nil()),
        )
    }

    fn executor(policy: Arc<dyn RuntimePolicy>) -> RuntimeExecutor {
        RuntimeExecutor::new(
            RuntimeTaskRegistry::default().register_task::<TestTask>(),
            policy,
            Arc::new(NoopRuntimeHooks),
        )
    }

    #[test]
    fn policy_decision_preserves_scope_and_defaults_only_absent_reasons() {
        let execution_id = Uuid::from_u128(1);
        let stage_id = Uuid::from_u128(2);
        let action_id = Uuid::from_u128(3);
        for (reason_code, reason_summary, expected_code, expected_summary) in [
            (None, None, "runtime_policy", "runtime policy decision"),
            (Some(""), Some(""), "", ""),
            (
                Some("model_denied"),
                Some("model request denied"),
                "model_denied",
                "model request denied",
            ),
        ] {
            let decision = build_policy_decision(
                execution_id,
                PolicyDecisionEntry {
                    target_kind: RuntimeDecisionTargetKind::ModelRequest,
                    decision_kind: RuntimeDecisionKind::Reject,
                    stage_record_id: Some(stage_id),
                    action_record_id: Some(action_id),
                    reason_code: reason_code.map(str::to_owned),
                    reason_summary_redacted: reason_summary.map(str::to_owned),
                },
            );
            assert_eq!(decision.runtime_execution_id, execution_id);
            assert_eq!(decision.stage_record_id, Some(stage_id));
            assert_eq!(decision.action_record_id, Some(action_id));
            assert_eq!(decision.target_kind, RuntimeDecisionTargetKind::ModelRequest);
            assert_eq!(decision.decision_kind, RuntimeDecisionKind::Reject);
            assert_eq!(decision.reason_code, expected_code);
            assert_eq!(decision.reason_summary_redacted, expected_summary);
        }
    }

    #[tokio::test]
    async fn records_allowed_stage_action_and_failed_terminal_outcome() {
        let executor = executor(Arc::new(AllowPolicy));
        let mut session = executor.seed_session(&request()).await.expect("seed session");
        let started_at = executor
            .begin_stage(&mut session, RuntimeStageKind::ExtractGraph)
            .await
            .expect("allow stage");
        let stage_id = executor.complete_stage(
            &mut session,
            StageCompletion {
                stage_kind: RuntimeStageKind::ExtractGraph,
                stage_state: RuntimeStageState::Completed,
                deterministic: false,
                failure_code: None,
                failure_summary_redacted: None,
                started_at,
            },
        );
        let action_id = executor
            .begin_action(
                &mut session,
                stage_id,
                RuntimeStageKind::ExtractGraph,
                RuntimeActionKind::ModelRequest,
            )
            .await
            .expect("allow model action");
        executor.complete_action(&mut session, action_id, RuntimeActionState::Completed);

        let result = executor
            .finalize_session::<TestTask>(
                session,
                RuntimeTerminalOutcome::Failed {
                    failure: "task_failed".to_string(),
                    summary: RuntimeFailureSummary {
                        code: "task_failed".to_string(),
                        summary_redacted: Some("task failed".to_string()),
                    },
                },
            )
            .await;

        assert_eq!(result.execution.lifecycle_state, RuntimeLifecycleState::Failed);
        assert_eq!(result.execution.turn_count, 1);
        assert_eq!(result.execution.failure_code.as_deref(), Some("task_failed"));
        assert!(result.execution.completed_at.is_some());
        assert_eq!(result.trace.stages[0].stage_state, RuntimeStageState::Completed);
        assert_eq!(result.trace.actions[0].action_state, RuntimeActionState::Completed);
        assert_eq!(result.trace.policy_decisions.len(), 4);
        assert_eq!(
            result.trace.policy_decisions[0].target_kind,
            RuntimeDecisionTargetKind::FinalOutcome
        );
    }

    #[tokio::test]
    async fn cancels_session_and_records_action_when_model_policy_rejects() {
        let executor = executor(Arc::new(RejectModelRequestPolicy));
        let mut session = executor.seed_session(&request()).await.expect("seed session");
        let stage_id = Uuid::now_v7();

        let error = executor
            .begin_action(
                &mut session,
                stage_id,
                RuntimeStageKind::ExtractGraph,
                RuntimeActionKind::ModelRequest,
            )
            .await
            .expect_err("reject model action");

        assert!(matches!(
            error,
            RuntimeExecutionError::PolicyBlocked {
                decision_kind: RuntimeDecisionKind::Reject,
                ref reason_code,
                ..
            } if reason_code == "model_denied"
        ));
        assert_eq!(session.execution.lifecycle_state, RuntimeLifecycleState::Canceled);
        assert_eq!(session.execution.turn_count, 1);
        assert_eq!(session.trace.actions.len(), 1);
        assert_eq!(session.trace.policy_decisions.len(), 2);
    }

    #[tokio::test]
    async fn replaces_terminal_outcome_when_final_policy_terminates() {
        let executor =
            executor(Arc::new(RejectFinalOutcomePolicy { final_outcome_calls: Mutex::new(0) }));

        let result = executor
            .execute(
                &request(),
                RuntimeTerminalOutcome::Completed { success: "completed".to_string() },
            )
            .await
            .expect("seed execution");

        assert_eq!(result.execution.lifecycle_state, RuntimeLifecycleState::Canceled);
        assert_eq!(result.execution.failure_code.as_deref(), Some("outcome_denied"));
        assert!(matches!(
            result.outcome,
            RuntimeTerminalOutcome::Canceled { ref failure, ref summary }
                if failure == "outcome_denied:final outcome denied"
                    && summary.code == "outcome_denied"
        ));
    }

    #[tokio::test]
    async fn rejects_stage_entry_after_session_becomes_terminal() {
        let executor = executor(Arc::new(AllowPolicy));
        let mut session = executor.seed_session(&request()).await.expect("seed session");
        session.execution.lifecycle_state = RuntimeLifecycleState::Failed;
        session.execution.failure_code = Some("already_failed".to_string());
        session.execution.failure_summary_redacted = Some("already failed".to_string());

        let error = executor
            .begin_stage(&mut session, RuntimeStageKind::ExtractGraph)
            .await
            .expect_err("terminal session must reject a new stage");

        assert!(matches!(
            error,
            RuntimeExecutionError::PolicyBlocked {
                decision_kind: RuntimeDecisionKind::Terminate,
                ref reason_code,
                ref reason_summary_redacted,
            } if reason_code == "already_failed" && reason_summary_redacted == "already failed"
        ));
        assert_eq!(session.trace.policy_decisions.len(), 1);
        assert_eq!(
            session.trace.policy_decisions[0].target_kind,
            RuntimeDecisionTargetKind::FinalOutcome
        );
        assert_eq!(session.trace.policy_decisions[0].decision_kind, RuntimeDecisionKind::Allow);
    }
}
