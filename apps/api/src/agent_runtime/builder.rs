use std::marker::PhantomData;

use crate::{
    agent_runtime::task::{RuntimeTaskRequest, StructuredRuntimeTask, TextRuntimeTask},
    domains::agent_runtime::{RuntimeExecutionOwner, RuntimeOverrideBudget, RuntimeSurfaceKind},
};

#[derive(Debug, Clone)]
pub struct StructuredRequestBuilder<TTask: StructuredRuntimeTask> {
    input: TTask::Input,
    execution_owner: RuntimeExecutionOwner,
    runtime_overrides: Option<RuntimeOverrideBudget>,
    surface_kind_override: Option<RuntimeSurfaceKind>,
    _task: PhantomData<TTask>,
}

impl<TTask: StructuredRuntimeTask> StructuredRequestBuilder<TTask> {
    #[must_use]
    pub const fn new(input: TTask::Input, execution_owner: RuntimeExecutionOwner) -> Self {
        Self {
            input,
            execution_owner,
            runtime_overrides: None,
            surface_kind_override: None,
            _task: PhantomData,
        }
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
    pub const fn with_surface_kind(mut self, surface_kind: RuntimeSurfaceKind) -> Self {
        self.surface_kind_override = Some(surface_kind);
        self
    }

    #[must_use]
    pub fn build(self) -> RuntimeTaskRequest<TTask> {
        let mut request = RuntimeTaskRequest::new(self.input, self.execution_owner);
        if let Some(surface_kind) = self.surface_kind_override {
            request = request.with_surface_kind(surface_kind);
        }
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
    surface_kind_override: Option<RuntimeSurfaceKind>,
    _task: PhantomData<TTask>,
}

impl<TTask: TextRuntimeTask> TextRequestBuilder<TTask> {
    #[must_use]
    pub const fn new(input: TTask::Input, execution_owner: RuntimeExecutionOwner) -> Self {
        Self {
            input,
            execution_owner,
            runtime_overrides: None,
            surface_kind_override: None,
            _task: PhantomData,
        }
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
    pub const fn with_surface_kind(mut self, surface_kind: RuntimeSurfaceKind) -> Self {
        self.surface_kind_override = Some(surface_kind);
        self
    }

    #[must_use]
    pub fn build(self) -> RuntimeTaskRequest<TTask> {
        let mut request = RuntimeTaskRequest::new(self.input, self.execution_owner);
        if let Some(surface_kind) = self.surface_kind_override {
            request = request.with_surface_kind(surface_kind);
        }
        match self.runtime_overrides {
            Some(runtime_overrides) => request.with_overrides(runtime_overrides),
            None => request,
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::{
        agent_runtime::{
            builder::TextRequestBuilder,
            tasks::query_answer::{QueryAnswerTask, QueryAnswerTaskInput},
        },
        domains::agent_runtime::{RuntimeExecutionOwner, RuntimeSurfaceKind},
    };

    #[test]
    fn text_runtime_request_preserves_surface_override() {
        let request = TextRequestBuilder::<QueryAnswerTask>::new(
            QueryAnswerTaskInput {
                query_execution_id: Uuid::from_u128(1),
                question: "Which documents are available?".to_string(),
                prompt_history_text: None,
                grounded_context_text: String::new(),
            },
            RuntimeExecutionOwner::query_execution(Uuid::from_u128(2)),
        )
        .with_surface_kind(RuntimeSurfaceKind::Ui)
        .build();

        assert_eq!(request.surface_kind_override, Some(RuntimeSurfaceKind::Ui));
    }
}
