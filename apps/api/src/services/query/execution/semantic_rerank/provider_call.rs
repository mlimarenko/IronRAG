use super::{ApiError, AppState, Duration, Result, Uuid};

pub(super) struct ProviderCallReservationGuard {
    state: AppState,
    provider_call_id: Uuid,
    terminal_state: &'static str,
    armed: bool,
}

impl ProviderCallReservationGuard {
    pub(super) fn new(state: &AppState, provider_call_id: Uuid) -> Self {
        Self { state: state.clone(), provider_call_id, terminal_state: "canceled", armed: true }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(super) fn mark_failed(&mut self) {
        self.terminal_state = "failed";
    }
}

impl Drop for ProviderCallReservationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                provider_call_id = %self.provider_call_id,
                "semantic rerank reservation dropped without an active cleanup runtime"
            );
            return;
        };
        let state = self.state.clone();
        let provider_call_id = self.provider_call_id;
        let terminal_state = self.terminal_state;
        runtime.spawn(async move {
            match state
                .application_services
                .billing
                .finish_reserved_provider_call_without_usage(
                    &state,
                    provider_call_id,
                    terminal_state,
                )
                .await
            {
                Ok(()) => tracing::warn!(
                    %provider_call_id,
                    terminal_state,
                    "reconciled semantic rerank billing reservation after task interruption"
                ),
                Err(error) => tracing::error!(
                    %provider_call_id,
                    terminal_state,
                    %error,
                    "failed to finalize interrupted semantic rerank billing reservation"
                ),
            }
        });
    }
}

pub(super) type ProviderCallReservationTask = tokio::task::JoinHandle<Result<Uuid, ApiError>>;

pub(super) struct PendingProviderCallReservationGuard {
    state: AppState,
    provider_call_id: Uuid,
    task: Option<ProviderCallReservationTask>,
}

impl PendingProviderCallReservationGuard {
    pub(super) fn new(
        state: &AppState,
        provider_call_id: Uuid,
        task: ProviderCallReservationTask,
    ) -> Self {
        Self { state: state.clone(), provider_call_id, task: Some(task) }
    }

    pub(super) async fn wait(
        &mut self,
        timeout: Duration,
    ) -> Option<Result<Result<Uuid, ApiError>, tokio::task::JoinError>> {
        let task = self.task.as_mut()?;
        let result = wait_for_provider_call_reservation(task, timeout).await?;
        let _ = self.task.take();
        Some(result)
    }
}

impl Drop for PendingProviderCallReservationGuard {
    fn drop(&mut self) {
        let Some(task) = self.task.take() else {
            return;
        };
        spawn_provider_call_reservation_reconciliation(
            &self.state,
            self.provider_call_id,
            Some(task),
        );
    }
}

pub(super) async fn wait_for_provider_call_reservation(
    task: &mut ProviderCallReservationTask,
    timeout: Duration,
) -> Option<Result<Result<Uuid, ApiError>, tokio::task::JoinError>> {
    tokio::time::timeout(timeout, task).await.ok()
}

pub(super) fn spawn_provider_call_reservation_reconciliation(
    state: &AppState,
    provider_call_id: Uuid,
    task: Option<ProviderCallReservationTask>,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::error!(
            %provider_call_id,
            "semantic rerank reservation could not schedule deadline reconciliation"
        );
        return;
    };
    let state = state.clone();
    runtime.spawn(async move {
        let returned_provider_call_id = match task {
            Some(task) => match task.await {
                Ok(Ok(returned_id)) => Some(returned_id),
                Ok(Err(error)) => {
                    tracing::warn!(
                        %provider_call_id,
                        %error,
                        "semantic rerank reservation finished with an error after its decision deadline"
                    );
                    None
                }
                Err(error) => {
                    tracing::error!(
                        %provider_call_id,
                        %error,
                        "semantic rerank reservation task failed after its decision deadline"
                    );
                    None
                }
            },
            None => None,
        };
        let mut reservation_ids = vec![provider_call_id];
        if returned_provider_call_id.is_some_and(|returned_id| returned_id != provider_call_id) {
            reservation_ids.extend(returned_provider_call_id);
        }
        for reservation_id in reservation_ids {
            match state
                .application_services
                .billing
                .cancel_reserved_provider_call_if_present(&state, reservation_id)
                .await
            {
                Ok(true) => tracing::warn!(
                    provider_call_id = %reservation_id,
                    "reconciled late semantic rerank billing reservation"
                ),
                Ok(false) => tracing::debug!(
                    provider_call_id = %reservation_id,
                    "late semantic rerank reservation did not commit"
                ),
                Err(error) => tracing::error!(
                    provider_call_id = %reservation_id,
                    %error,
                    "failed to reconcile late semantic rerank billing reservation"
                ),
            }
        }
    });
}
