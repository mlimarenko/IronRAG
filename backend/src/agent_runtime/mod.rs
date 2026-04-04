pub mod builder;
pub mod default_policy;
pub mod executor;
pub mod hooks;
pub mod persistence;
pub mod pipeline;
pub mod policy;
pub mod registry;
pub mod request;
pub mod response;
pub mod task;
pub mod tasks;
pub mod trace;

use std::sync::Arc;

use sqlx::PgPool;

use default_policy::{DefaultRuntimePolicy, DefaultRuntimePolicyRules};
use executor::RuntimeExecutor;
use hooks::{NoopRuntimeHooks, RuntimeHooks};
use policy::RuntimePolicy;
use registry::RuntimeTaskRegistry;
use task::RuntimeTaskRequest;

#[derive(Clone)]
pub struct AgentRuntime {
    registry: RuntimeTaskRegistry,
    executor: RuntimeExecutor,
    policy: Arc<dyn RuntimePolicy>,
    hooks: Arc<dyn RuntimeHooks>,
}

impl AgentRuntime {
    #[must_use]
    pub fn new(
        registry: RuntimeTaskRegistry,
        policy: Arc<dyn RuntimePolicy>,
        hooks: Arc<dyn RuntimeHooks>,
    ) -> Self {
        let executor =
            RuntimeExecutor::new(registry.clone(), Arc::clone(&policy), Arc::clone(&hooks));
        Self { registry, executor, policy, hooks }
    }

    #[must_use]
    pub fn with_defaults() -> Self {
        let registry = RuntimeTaskRegistry::default();
        let policy: Arc<dyn RuntimePolicy> =
            Arc::new(DefaultRuntimePolicy::new(2_000, DefaultRuntimePolicyRules::default()));
        let hooks: Arc<dyn RuntimeHooks> = Arc::new(NoopRuntimeHooks);
        Self::new(registry, policy, hooks)
    }

    #[must_use]
    pub const fn registry(&self) -> &RuntimeTaskRegistry {
        &self.registry
    }

    #[must_use]
    pub const fn executor(&self) -> &RuntimeExecutor {
        &self.executor
    }

    /// # Errors
    /// Returns an error when the runtime request is invalid or the seeded execution cannot be
    /// persisted as the canonical runtime owner record.
    pub async fn seed_and_persist_session<T>(
        &self,
        pool: &PgPool,
        request: &RuntimeTaskRequest<T>,
    ) -> Result<executor::RuntimeExecutionSession, executor::RuntimeExecutionError>
    where
        T: task::RuntimeTask,
    {
        let session = self.executor.seed_session(request).await?;
        persistence::create_runtime_execution(pool, &session.execution).await.map_err(|error| {
            executor::RuntimeExecutionError::InvalidTaskSpec(format!(
                "failed to persist runtime execution {}: {error}",
                session.execution.id
            ))
        })?;
        Ok(session)
    }

    #[must_use]
    pub fn policy(&self) -> Arc<dyn RuntimePolicy> {
        Arc::clone(&self.policy)
    }

    #[must_use]
    pub fn hooks(&self) -> Arc<dyn RuntimeHooks> {
        Arc::clone(&self.hooks)
    }
}
