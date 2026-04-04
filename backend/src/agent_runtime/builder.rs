use std::marker::PhantomData;

use crate::{
    agent_runtime::task::{RuntimeTaskRequest, StructuredRuntimeTask, TextRuntimeTask},
    domains::agent_runtime::{RuntimeExecutionOwner, RuntimeOverrideBudget},
};

#[derive(Debug, Clone)]
pub struct StructuredRequestBuilder<TTask: StructuredRuntimeTask> {
    input: TTask::Input,
    execution_owner: RuntimeExecutionOwner,
    runtime_overrides: Option<RuntimeOverrideBudget>,
    _task: PhantomData<TTask>,
}

impl<TTask: StructuredRuntimeTask> StructuredRequestBuilder<TTask> {
    #[must_use]
    pub const fn new(input: TTask::Input, execution_owner: RuntimeExecutionOwner) -> Self {
        Self { input, execution_owner, runtime_overrides: None, _task: PhantomData }
    }

    #[must_use]
    pub const fn with_budget_limits(
        mut self,
        max_turns: Option<u8>,
        max_parallel_actions: Option<u8>,
    ) -> Self {
        self.runtime_overrides = Some(RuntimeOverrideBudget { max_turns, max_parallel_actions });
        self
    }

    #[must_use]
    pub fn build(self) -> RuntimeTaskRequest<TTask> {
        let request = RuntimeTaskRequest::new(self.input, self.execution_owner);
        match self.runtime_overrides {
            Some(runtime_overrides) => request.with_overrides(runtime_overrides),
            None => request,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextRequestBuilder<TTask: TextRuntimeTask> {
    input: TTask::Input,
    execution_owner: RuntimeExecutionOwner,
    runtime_overrides: Option<RuntimeOverrideBudget>,
    _task: PhantomData<TTask>,
}

impl<TTask: TextRuntimeTask> TextRequestBuilder<TTask> {
    #[must_use]
    pub const fn new(input: TTask::Input, execution_owner: RuntimeExecutionOwner) -> Self {
        Self { input, execution_owner, runtime_overrides: None, _task: PhantomData }
    }

    #[must_use]
    pub const fn with_budget_limits(
        mut self,
        max_turns: Option<u8>,
        max_parallel_actions: Option<u8>,
    ) -> Self {
        self.runtime_overrides = Some(RuntimeOverrideBudget { max_turns, max_parallel_actions });
        self
    }

    #[must_use]
    pub fn build(self) -> RuntimeTaskRequest<TTask> {
        let request = RuntimeTaskRequest::new(self.input, self.execution_owner);
        match self.runtime_overrides {
            Some(runtime_overrides) => request.with_overrides(runtime_overrides),
            None => request,
        }
    }
}
