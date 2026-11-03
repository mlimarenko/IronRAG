use uuid::Uuid;

/// Durable query and runtime ownership established before provider admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryProviderExecutionContext {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) query_execution_id: Uuid,
    pub(crate) runtime_execution_id: Uuid,
}
