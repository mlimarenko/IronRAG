pub mod graph_extract;
pub mod query_answer;
pub mod query_plan;
pub mod query_rerank;
pub mod query_verify;
pub mod structured_prepare;
pub mod technical_fact_extract;

use crate::agent_runtime::registry::RuntimeTaskRegistry;

pub type RuntimeTaskCatalogExtension = fn(RuntimeTaskRegistry) -> RuntimeTaskRegistry;

fn register_builtin_task_catalog(registry: &RuntimeTaskRegistry) -> RuntimeTaskRegistry {
    registry
        .clone()
        .register_task::<query_plan::QueryPlanTask>()
        .register_task::<query_rerank::QueryRerankTask>()
        .register_task::<query_answer::QueryAnswerTask>()
        .register_task::<query_verify::QueryVerifyTask>()
        .register_task::<graph_extract::GraphExtractTask>()
        .register_task::<structured_prepare::StructuredPrepareTask>()
        .register_task::<technical_fact_extract::TechnicalFactExtractTask>()
}

#[must_use]
pub fn register_task_catalog(registry: &RuntimeTaskRegistry) -> RuntimeTaskRegistry {
    register_task_catalog_extensions(registry, &[])
}

pub fn register_task_catalog_extensions(
    registry: &RuntimeTaskRegistry,
    extensions: &[RuntimeTaskCatalogExtension],
) -> RuntimeTaskRegistry {
    let mut catalog = register_builtin_task_catalog(registry);
    for extension in extensions {
        catalog = extension(catalog);
    }
    catalog
}
