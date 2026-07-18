use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{agent_runtime::RuntimeTaskKind, ai::AiBindingPurpose},
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ops::billing::ReserveExecutionProviderCallCommand,
    },
};

/// Durable ownership shared by every paid provider call made while executing
/// one query. The query row and its runtime execution are created before any
/// provider I/O, so billing can reserve a stable event id before the request
/// leaves the process instead of reconstructing calls from a terminal result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryProviderExecutionContext {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) query_execution_id: Uuid,
    pub(crate) runtime_execution_id: Uuid,
}

/// A durable provider-call reservation that must be completed with the exact
/// response usage before the response may advance through the query pipeline.
///
/// Before a response, dropping the guard schedules a conservative terminal
/// transition. After a response, the guard owns the exact usage payload and
/// dropping it schedules idempotent completion retries; a paid response is
/// never downgraded to a no-usage failure.
pub(crate) struct QueryProviderCallReservation {
    state: AppState,
    provider_call_id: Uuid,
    recovery: Option<QueryProviderCallRecovery>,
}

enum QueryProviderCallRecovery {
    PreResponse { terminal_state: &'static str },
    ResponseObserved { usage_json: serde_json::Value },
}

pub(crate) type QueryProviderCallCompletionTask = tokio::task::JoinHandle<Result<(), ApiError>>;

/// Owns an in-flight canonical completion plus the exact response usage needed
/// to retry it after a deadline, cancellation, join failure, or database error.
pub(crate) struct PendingQueryProviderCallCompletion {
    state: AppState,
    provider_call_id: Uuid,
    task: Option<QueryProviderCallCompletionTask>,
    usage_json: Option<serde_json::Value>,
}

impl PendingQueryProviderCallCompletion {
    pub(crate) fn new(
        state: &AppState,
        provider_call_id: Uuid,
        task: QueryProviderCallCompletionTask,
        usage_json: serde_json::Value,
    ) -> Self {
        Self {
            state: state.clone(),
            provider_call_id,
            task: Some(task),
            usage_json: Some(usage_json),
        }
    }

    pub(crate) async fn wait(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<Result<Result<(), ApiError>, tokio::task::JoinError>> {
        let task = self.task.as_mut()?;
        let result = tokio::time::timeout(timeout, task).await.ok()?;
        let _ = self.task.take();
        if matches!(&result, Ok(Ok(()))) {
            let _ = self.usage_json.take();
        }
        Some(result)
    }
}

impl Drop for PendingQueryProviderCallCompletion {
    fn drop(&mut self) {
        let Some(usage_json) = self.usage_json.take() else {
            return;
        };
        spawn_known_usage_completion_reconciliation(
            &self.state,
            self.provider_call_id,
            self.task.take(),
            usage_json,
        );
    }
}

fn spawn_known_usage_completion_reconciliation(
    state: &AppState,
    provider_call_id: Uuid,
    task: Option<QueryProviderCallCompletionTask>,
    usage_json: serde_json::Value,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::error!(
            %provider_call_id,
            "known provider usage could not schedule completion reconciliation"
        );
        return;
    };
    let state = state.clone();
    runtime.spawn(async move {
        let needs_completion_retry = match task {
            Some(task) => match task.await {
                Ok(Ok(())) => false,
                Ok(Err(error)) => {
                    tracing::warn!(
                        %provider_call_id,
                        %error,
                        "provider usage completion failed before background reconciliation"
                    );
                    true
                }
                Err(error) => {
                    tracing::error!(
                        %provider_call_id,
                        %error,
                        "provider usage completion task failed before background reconciliation"
                    );
                    true
                }
            },
            None => true,
        };
        if needs_completion_retry
            && let Err(error) = state
                .canonical_services
                .billing
                .complete_reserved_provider_call_deferred_rollup_with_retry(
                    &state,
                    provider_call_id,
                    &usage_json,
                )
                .await
        {
            tracing::error!(
                %provider_call_id,
                %error,
                "known provider usage remained uncommitted after bounded reconciliation; reservation stays started for operator repair"
            );
        }
    });
}

impl QueryProviderCallReservation {
    pub(crate) async fn reserve(
        state: &AppState,
        context: QueryProviderExecutionContext,
        binding: &ResolvedRuntimeBinding,
        expected_binding_purpose: AiBindingPurpose,
        call_kind: &str,
    ) -> Result<Self, ApiError> {
        if binding.binding_purpose != expected_binding_purpose {
            return Err(ApiError::Conflict(format!(
                "provider binding {} has purpose {}, expected {} for {call_kind}",
                binding.binding_id,
                binding.binding_purpose.as_str(),
                expected_binding_purpose.as_str(),
            )));
        }
        if binding.library_id != context.library_id || binding.workspace_id != context.workspace_id
        {
            return Err(ApiError::Conflict(format!(
                "provider binding {} does not match query execution scope",
                binding.binding_id
            )));
        }

        let provider_call_id = Uuid::now_v7();
        state
            .canonical_services
            .billing
            .reserve_execution_provider_call_with_id(
                state,
                provider_call_id,
                ReserveExecutionProviderCallCommand {
                    workspace_id: context.workspace_id,
                    library_id: context.library_id,
                    owning_execution_kind: "query_execution".to_string(),
                    owning_execution_id: context.query_execution_id,
                    runtime_execution_id: Some(context.runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer),
                    binding_id: Some(binding.binding_id),
                    provider_catalog_id: binding.provider_catalog_id,
                    model_catalog_id: binding.model_catalog_id,
                    call_kind: call_kind.to_string(),
                },
            )
            .await?;

        Ok(Self {
            state: state.clone(),
            provider_call_id,
            recovery: Some(QueryProviderCallRecovery::PreResponse { terminal_state: "canceled" }),
        })
    }

    #[must_use]
    pub(crate) const fn provider_call_id(&self) -> Uuid {
        self.provider_call_id
    }

    /// Persist canonical usage and the completed state before any caller may
    /// persist a terminal query outcome. Derived cost repair is intentionally
    /// deferred; the canonical completion transaction is awaited here.
    pub(crate) async fn complete(
        &mut self,
        usage_json: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let capture_usage = match self.recovery.as_ref() {
            None => return Ok(()),
            Some(QueryProviderCallRecovery::ResponseObserved { usage_json: observed_usage })
                if observed_usage != usage_json =>
            {
                return Err(ApiError::Conflict(format!(
                    "provider call {} cannot be completed with two different usage payloads",
                    self.provider_call_id
                )));
            }
            Some(QueryProviderCallRecovery::PreResponse { .. }) => true,
            Some(QueryProviderCallRecovery::ResponseObserved { .. }) => false,
        };
        if capture_usage {
            // Own the exact response usage before the first fallible
            // completion attempt. From this point onward, cancellation or a
            // transient database failure must retry completion and may never
            // downgrade the paid response to a no-usage failure.
            self.recovery = Some(QueryProviderCallRecovery::ResponseObserved {
                usage_json: usage_json.clone(),
            });
        }
        self.state
            .canonical_services
            .billing
            .complete_reserved_provider_call_deferred_rollup_with_retry(
                &self.state,
                self.provider_call_id,
                usage_json,
            )
            .await?;
        self.recovery = None;
        Ok(())
    }

    /// Mark a request that failed before returning provider usage. This is
    /// awaited on ordinary error paths; the drop fallback retries after an
    /// interrupted or ambiguous database operation.
    pub(crate) async fn fail(&mut self) -> Result<(), ApiError> {
        match self.recovery.as_mut() {
            None => return Ok(()),
            Some(QueryProviderCallRecovery::ResponseObserved { .. }) => {
                return Err(ApiError::Conflict(format!(
                    "provider call {} already observed a response and must retain its usage",
                    self.provider_call_id
                )));
            }
            Some(QueryProviderCallRecovery::PreResponse { terminal_state }) => {
                *terminal_state = "failed";
            }
        }
        self.state
            .canonical_services
            .billing
            .finish_reserved_provider_call_without_usage(
                &self.state,
                self.provider_call_id,
                "failed",
            )
            .await?;
        self.recovery = None;
        Ok(())
    }
}

impl Drop for QueryProviderCallReservation {
    fn drop(&mut self) {
        let Some(recovery) = self.recovery.take() else {
            return;
        };
        let terminal_state = match recovery {
            QueryProviderCallRecovery::ResponseObserved { usage_json } => {
                spawn_known_usage_completion_reconciliation(
                    &self.state,
                    self.provider_call_id,
                    None,
                    usage_json,
                );
                return;
            }
            QueryProviderCallRecovery::PreResponse { terminal_state } => terminal_state,
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                provider_call_id = %self.provider_call_id,
                "pre-response provider-call reservation dropped without a reconciliation runtime"
            );
            return;
        };
        let state = self.state.clone();
        let provider_call_id = self.provider_call_id;
        runtime.spawn(async move {
            if let Err(error) = state
                .canonical_services
                .billing
                .finish_reserved_provider_call_without_usage(
                    &state,
                    provider_call_id,
                    terminal_state,
                )
                .await
            {
                tracing::error!(
                    %provider_call_id,
                    terminal_state,
                    %error,
                    "failed to reconcile interrupted pre-response provider-call reservation"
                );
            }
        });
    }
}
