use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    agent_runtime::{response::RuntimeTerminalOutcome, trace::RuntimeExecutionTraceView},
    domains::{
        agent_runtime::{
            RuntimeExecution, RuntimeExecutionOwner, RuntimeOverrideBudget, RuntimeRecoveryPolicy,
            RuntimeStageKind, RuntimeSurfaceKind, RuntimeTaskKind,
        },
        ai::AiBindingPurpose,
    },
};

#[derive(Debug, Clone)]
pub struct RuntimeTaskSpec {
    pub task_kind: RuntimeTaskKind,
    pub surface_kind: RuntimeSurfaceKind,
    pub binding_purpose: Option<AiBindingPurpose>,
    pub machine_consumed: bool,
    pub max_turns: u8,
    pub max_parallel_actions: u8,
    pub stage_catalog: &'static [RuntimeStageKind],
    pub recovery_policy: RuntimeRecoveryPolicy,
    pub output_mode: crate::domains::agent_runtime::RuntimeOutputMode,
}

impl RuntimeTaskSpec {
    /// # Errors
    /// Returns an error when the task contract declares an invalid runtime budget, output mode,
    /// or empty stage catalog.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_turns == 0 {
            return Err("runtime task max_turns must be at least 1".to_string());
        }
        if self.max_parallel_actions == 0 {
            return Err("runtime task max_parallel_actions must be at least 1".to_string());
        }
        if self.machine_consumed
            && !matches!(
                self.output_mode,
                crate::domains::agent_runtime::RuntimeOutputMode::Structured
            )
        {
            return Err(
                "machine-consumed runtime tasks must declare structured output mode".to_string()
            );
        }
        if self.stage_catalog.is_empty() {
            return Err("runtime task stage catalog must not be empty".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeTaskRequest<TTask: RuntimeTask> {
    pub input: TTask::Input,
    pub execution_owner: RuntimeExecutionOwner,
    pub contract_name: &'static str,
    pub contract_version: &'static str,
    pub runtime_overrides: Option<RuntimeOverrideBudget>,
    _task: PhantomData<TTask>,
}

impl<TTask: RuntimeTask> RuntimeTaskRequest<TTask> {
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(input: TTask::Input, execution_owner: RuntimeExecutionOwner) -> Self {
        Self {
            input,
            execution_owner,
            contract_name: TTask::CONTRACT_NAME,
            contract_version: TTask::CONTRACT_VERSION,
            runtime_overrides: None,
            _task: PhantomData,
        }
    }

    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn with_overrides(mut self, runtime_overrides: RuntimeOverrideBudget) -> Self {
        self.runtime_overrides = Some(runtime_overrides);
        self
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeTaskResult<TTask: RuntimeTask> {
    pub execution: RuntimeExecution,
    pub trace: RuntimeExecutionTraceView,
    pub outcome: RuntimeTerminalOutcome<TTask::Success, TTask::Failure>,
}

pub trait RuntimeTask: Send + Sync + 'static {
    type Input: Clone + Send + Sync + Serialize + DeserializeOwned + 'static;
    type Success: Clone + Send + Sync + Serialize + DeserializeOwned + 'static;
    type Failure: Clone + Send + Sync + Serialize + DeserializeOwned + 'static;

    const CONTRACT_NAME: &'static str;
    const CONTRACT_VERSION: &'static str;

    fn spec() -> RuntimeTaskSpec;

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure;
}

pub trait StructuredRuntimeTask: RuntimeTask {}

pub trait TextRuntimeTask: RuntimeTask {}
