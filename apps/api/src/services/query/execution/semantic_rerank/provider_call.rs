use std::time::Duration;

use super::{AppState, Uuid};
use crate::shared::provider_send::ProviderSendScope;

/// Terminalizes the send rows of one semantic rerank call when its task is
/// interrupted or times out. The owning accounting path stays authoritative:
/// this guard only transitions rows that are still `started`, and a row that
/// already completed keeps its state.
pub(super) struct ProviderCallReservationGuard {
    state: AppState,
    /// The logical call's ambient scope: attribution for every physical send and
    /// every durably admitted row this logical call produced.
    scope: ProviderSendScope,
    terminal_state: &'static str,
    armed: bool,
}

impl ProviderCallReservationGuard {
    pub(super) fn new(state: &AppState, scope: ProviderSendScope) -> Self {
        Self { state: state.clone(), scope, terminal_state: "canceled", armed: true }
    }

    /// The scope to install around the provider dispatch of this call.
    pub(super) fn send_scope(&self) -> ProviderSendScope {
        self.scope.clone()
    }

    /// The row that produced the reply, once a send was admitted.
    pub(super) fn current_send_id(&self) -> Option<Uuid> {
        self.scope.current_send_id()
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(super) fn mark_failed(&mut self) {
        self.terminal_state = "failed";
    }

    /// Explicitly settles every durably admitted send row of this logical call
    /// without fabricating usage, bounded by `budget`.
    ///
    /// Returns `Ok(())` only when:
    /// - no admission in this scope had an uncertain outcome (commit-ack loss,
    ///   persistence error, or mid-admission cancellation), AND
    /// - every admitted send row was durably observed/transitioned to a
    ///   terminal state (`canceled` / `failed` / already finalized) within
    ///   `budget`.
    ///
    /// Otherwise returns `Err(())` so the caller stays fail-closed (`Integrity`).
    /// On success the guard is disarmed because every row is already settled.
    pub(super) async fn settle_after_timeout(&mut self, budget: Duration) -> Result<(), ()> {
        if self.scope.has_uncertain_admission() {
            return Err(());
        }
        let send_ids = self.scope.published_send_ids();
        if send_ids.is_empty() {
            self.disarm();
            return Ok(());
        }
        if budget.is_zero() {
            return Err(());
        }
        let settle_all = async {
            for provider_call_id in &send_ids {
                self.state
                    .application_services
                    .billing
                    .finish_reserved_provider_call_without_usage(
                        &self.state,
                        *provider_call_id,
                        "canceled",
                    )
                    .await
                    .map_err(|error| {
                        tracing::error!(
                            %provider_call_id,
                            %error,
                            "failed to settle timed-out semantic rerank provider call"
                        );
                    })?;
            }
            Ok(())
        };
        match tokio::time::timeout(budget, settle_all).await {
            Ok(Ok(())) => {
                self.disarm();
                Ok(())
            }
            Ok(Err(())) | Err(_) => Err(()),
        }
    }
}

impl Drop for ProviderCallReservationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // A logical call whose sends were all refused owns no row: nothing was
        // debited and nothing must be terminalized.
        let send_ids = self.scope.published_send_ids();
        if send_ids.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                send_count = send_ids.len(),
                "semantic rerank reservation dropped without an active cleanup runtime"
            );
            return;
        };
        let state = self.state.clone();
        let terminal_state = self.terminal_state;
        runtime.spawn(async move {
            for provider_call_id in send_ids {
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
            }
        });
    }
}
