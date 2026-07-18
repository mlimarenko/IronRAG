use uuid::Uuid;

use crate::{
    agent_runtime::task::RuntimeTaskSpec, app::state::AppState,
    domains::agent_runtime::RuntimeOverrideBudget,
    services::ai_catalog_service::ResolvedRuntimeBinding,
    services::ingest::error::IngestServiceError,
};

#[derive(Debug, Clone)]
pub(crate) struct RuntimeTaskExecutionContext {
    pub runtime_binding: ResolvedRuntimeBinding,
    pub runtime_overrides: RuntimeOverrideBudget,
}

#[must_use]
pub(crate) fn bounded_runtime_overrides(
    state: &AppState,
    task_spec: &RuntimeTaskSpec,
) -> RuntimeOverrideBudget {
    RuntimeOverrideBudget {
        max_turns: Some(state.agent_runtime_settings.max_turns.min(task_spec.max_turns)),
        max_parallel_actions: Some(
            state.agent_runtime_settings.max_parallel_actions.min(task_spec.max_parallel_actions),
        ),
    }
}

pub(crate) async fn resolve_runtime_task_execution_context(
    state: &AppState,
    library_id: Uuid,
    task_spec: &RuntimeTaskSpec,
) -> Result<RuntimeTaskExecutionContext, IngestServiceError> {
    let binding_purpose =
        task_spec.binding_purpose.ok_or_else(|| IngestServiceError::BindingNotConfigured {
            message: format!(
                "runtime task {} does not declare a provider binding",
                task_spec.task_kind.as_str()
            ),
        })?;
    let runtime_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, binding_purpose)
        .await?
        .ok_or_else(|| IngestServiceError::BindingNotConfigured {
            message: format!(
                "active {} binding is not configured for library {library_id}",
                binding_purpose.as_str()
            ),
        })?;

    Ok(RuntimeTaskExecutionContext {
        runtime_binding,
        runtime_overrides: bounded_runtime_overrides(state, task_spec),
    })
}
