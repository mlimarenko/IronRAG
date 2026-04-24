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
        RuntimeStageKind, RuntimeStageRecord, RuntimeStageState,
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
    #[allow(clippy::missing_const_for_fn)]
    pub fn consume_turn(&mut self) -> Result<(), RuntimeExecutionError> {
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

    pub fn record_policy_decision(
        &mut self,
        target_kind: RuntimeDecisionTargetKind,
        decision_kind: RuntimeDecisionKind,
        reason_code: Option<String>,
        reason_summary_redacted: Option<String>,
        stage_record_id: Option<Uuid>,
        action_record_id: Option<Uuid>,
    ) {
        self.trace.policy_decisions.push(RuntimePolicyDecision {
            id: Uuid::now_v7(),
            runtime_execution_id: self.execution.id,
            stage_record_id,
            action_record_id,
            target_kind,
            decision_kind,
            reason_code: reason_code.unwrap_or_else(|| "runtime_policy".to_string()),
            reason_summary_redacted: reason_summary_redacted
                .unwrap_or_else(|| "runtime policy decision".to_string()),
            created_at: Utc::now(),
        });
    }
}

fn build_policy_decision(
    execution_id: Uuid,
    target_kind: RuntimeDecisionTargetKind,
    decision_kind: RuntimeDecisionKind,
    reason_code: Option<String>,
    reason_summary_redacted: Option<String>,
    stage_record_id: Option<Uuid>,
    action_record_id: Option<Uuid>,
) -> RuntimePolicyDecision {
    RuntimePolicyDecision {
        id: Uuid::now_v7(),
        runtime_execution_id: execution_id,
        stage_record_id,
        action_record_id,
        target_kind,
        decision_kind,
        reason_code: reason_code.unwrap_or_else(|| "runtime_policy".to_string()),
        reason_summary_redacted: reason_summary_redacted
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
        execution_id: Uuid,
        task_kind: crate::domains::agent_runtime::RuntimeTaskKind,
        stage_kind: Option<RuntimeStageKind>,
        action_kind: Option<RuntimeActionKind>,
        target_kind: RuntimeDecisionTargetKind,
        reason_code: Option<String>,
    ) -> crate::agent_runtime::policy::RuntimePolicyOutcome {
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
    /// and passes it back to [`complete_stage`] so the trace row
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
        // Capture the real stage-entry timestamp BEFORE the policy
        // round-trip — evaluate_target can take several ms on some
        // policies, and attributing that setup cost to the stage
        // itself is exactly the kind of fudge the old "zero-diff"
        // trace row was hiding.
        let started_at = Utc::now();
        let decision = self
            .evaluate_target(
                session.execution.id,
                session.execution.task_kind,
                Some(stage_kind),
                None,
                RuntimeDecisionTargetKind::StageTransition,
                Some(format!("stage_transition:{}", stage_kind.as_str())),
            )
            .await;
        session.record_policy_decision(
            RuntimeDecisionTargetKind::StageTransition,
            decision.decision_kind,
            decision.reason_code.clone(),
            decision.reason_summary_redacted.clone(),
            None,
            None,
        );
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
        stage_kind: RuntimeStageKind,
        stage_state: RuntimeStageState,
        deterministic: bool,
        failure_code: Option<String>,
        failure_summary_redacted: Option<String>,
        started_at: DateTime<Utc>,
    ) -> Uuid {
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
                .evaluate_target(
                    session.execution.id,
                    session.execution.task_kind,
                    Some(stage_kind),
                    Some(action_kind),
                    target_kind,
                    Some(format!("action:{}", action_kind.as_str())),
                )
                .await;
            let action_id =
                session.record_action(stage_record_id, action_kind, RuntimeActionState::Running);
            session.record_policy_decision(
                target_kind,
                decision.decision_kind,
                decision.reason_code.clone(),
                decision.reason_summary_redacted.clone(),
                Some(stage_record_id),
                Some(action_id),
            );
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
            .evaluate_target(
                execution_id,
                spec.task_kind,
                None,
                None,
                RuntimeDecisionTargetKind::FinalOutcome,
                Some("execution_acceptance".to_string()),
            )
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
                surface_kind: spec.surface_kind,
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
        let mut trace = RuntimeExecutionTraceView::new(execution.clone());
        trace.policy_decisions.push(build_policy_decision(
            execution.id,
            RuntimeDecisionTargetKind::FinalOutcome,
            decision.decision_kind,
            decision.reason_code,
            decision.reason_summary_redacted,
            None,
            None,
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
            .evaluate_target(
                session.execution.id,
                session.execution.task_kind,
                session.execution.active_stage,
                None,
                RuntimeDecisionTargetKind::FinalOutcome,
                Some(final_outcome_reason_code(&outcome)),
            )
            .await;
        session.record_policy_decision(
            RuntimeDecisionTargetKind::FinalOutcome,
            decision.decision_kind,
            decision.reason_code.clone(),
            decision.reason_summary_redacted.clone(),
            None,
            None,
        );
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
