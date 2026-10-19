use uuid::Uuid;

use crate::{
    agent_runtime::task::RuntimeTaskSpec,
    app::state::AppState,
    domains::{agent_runtime::RuntimeOverrideBudget, ai::AiBindingPurpose},
    services::ai_catalog_service::ResolvedRuntimeBinding,
    services::ingest::error::IngestServiceError,
};

#[derive(Debug, Clone)]
pub(crate) struct RuntimeTaskExecutionContext {
    pub runtime_binding: ResolvedRuntimeBinding,
    pub runtime_overrides: RuntimeOverrideBudget,
    /// The optional `graph_entity_type` binding that types extracted entity
    /// candidates before identity computation. Resolved once here so the
    /// extraction cache fingerprint and the paid classifier call share one
    /// binding state. Always `None` for tasks other than graph extraction.
    pub entity_type_binding: Option<ResolvedRuntimeBinding>,
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
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, binding_purpose)
        .await?
        .ok_or_else(|| IngestServiceError::BindingNotConfigured {
            message: format!(
                "active {} binding is not configured for library {library_id}",
                binding_purpose.as_str()
            ),
        })?;

    // The optional entity-typing binding is resolved for graph extraction only,
    // and its absence is the explicit `off` mode, not an error. A binding that
    // drifted out of eligibility must not fail extraction either: the stage is
    // optional, so it resolves to `off` with a warning recorded.
    let entity_type_binding = if binding_purpose == AiBindingPurpose::ExtractGraph {
        match state
            .application_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::GraphEntityType)
            .await
        {
            Ok(binding) => binding,
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    %error,
                    "graph_entity_type binding did not resolve; entity typing stays off for this extraction"
                );
                None
            }
        }
    } else {
        None
    };

    Ok(RuntimeTaskExecutionContext {
        runtime_binding,
        runtime_overrides: bounded_runtime_overrides(state, task_spec),
        entity_type_binding,
    })
}
