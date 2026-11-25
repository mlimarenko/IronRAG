//! The one ambient scope of a logical provider call.
//!
//! A logical call may issue several physical sends because the transport retry
//! loop re-sends after a retryable failure. Every admitted send owns exactly one
//! `billing_provider_call` row, created by the durable admission owner in the
//! same transaction that debits the financial counter. This module carries the
//! two facts the send boundary cannot derive on its own — where the send belongs
//! (the attribution) and which row the reply belongs to (the current row) — and
//! nothing else. It holds no secret and performs no I/O.

use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard},
};

use uuid::Uuid;

/// Where one physical send belongs in the durable provider-call ledger.
///
/// It is built by the domain that owns the logical call from the same resolved
/// binding and the same owning execution the caller already names, so the
/// per-send rows keep the exact owner, purpose and document scope the logical
/// call has today.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSendAttribution {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub call_kind: String,
    pub binding_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub runtime_task_kind: Option<&'static str>,
}

/// Why a send could not claim a durable row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSendClaimError {
    /// The logical call has no ambient attribution, so no row could be named.
    NoAttribution,
    /// Another send of the same logical call is still in flight. A logical call
    /// settles one reply, so two concurrent sends in one scope are refused
    /// instead of silently settling each other's row.
    Busy,
}

impl std::fmt::Display for ProviderSendClaimError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAttribution => formatter.write_str("provider send has no attribution"),
            Self::Busy => formatter.write_str("provider send is already in flight in this scope"),
        }
    }
}

#[derive(Debug)]
struct ProviderSendState {
    attribution: ProviderSendAttribution,
    logical_id: Uuid,
    /// The row of the most recent send. It is what the caller settles when the
    /// reply arrives, and it keeps pointing at a terminal row after a failed
    /// attempt so a late settlement attempt cannot invent a different one.
    current: Option<Uuid>,
    /// Every send row that was durably admitted and published in this scope, in
    /// admission order (including retried physical attempts).
    published: Vec<Uuid>,
    /// The row whose transport is still running, if any.
    in_flight: Option<Uuid>,
    /// Set when an admission attempt entered the durable write path and did not
    /// return a confirmed rollback/refusal (e.g., persistence/commit-ack error,
    /// or the admission future was dropped mid-flight before publishing or
    /// releasing). Any caller observing `admission_uncertain == true` must stay
    /// fail-closed.
    admission_uncertain: bool,
}

/// Per logical call holder of the attribution and of the current send row.
#[derive(Clone, Debug)]
pub struct ProviderSendScope(Arc<Mutex<ProviderSendState>>);

impl ProviderSendScope {
    #[must_use]
    pub fn new(attribution: ProviderSendAttribution, logical_id: Uuid) -> Self {
        Self(Arc::new(Mutex::new(ProviderSendState {
            attribution,
            logical_id,
            current: None,
            published: Vec::new(),
            in_flight: None,
            admission_uncertain: false,
        })))
    }

    /// The id the caller generated for this logical call. The first send of the
    /// call reuses it as its own row id, so a call that is never retried keeps
    /// the trace and ledger identity it had before the per-send split.
    #[must_use]
    pub fn logical_id(&self) -> Uuid {
        self.lock().logical_id
    }

    /// The row that produced (or is producing) the current reply.
    #[must_use]
    pub fn current_send_id(&self) -> Option<Uuid> {
        self.lock().current
    }

    /// All durably admitted send rows recorded in this logical call's scope,
    /// including any retried physical attempts.
    #[must_use]
    pub fn published_send_ids(&self) -> Vec<Uuid> {
        self.lock().published.clone()
    }

    /// Whether any admission attempt in this scope has an uncertain durable
    /// outcome (commit-ack loss, persistence error, or dropped mid-admission).
    #[must_use]
    pub fn has_uncertain_admission(&self) -> bool {
        let state = self.lock();
        state.admission_uncertain || (state.in_flight.is_some() && state.in_flight != state.current)
    }

    #[must_use]
    pub fn attribution(&self) -> ProviderSendAttribution {
        self.lock().attribution.clone()
    }

    fn claim(&self) -> Result<Uuid, ProviderSendClaimError> {
        let mut state = self.lock();
        if state.in_flight.is_some() {
            return Err(ProviderSendClaimError::Busy);
        }
        let provider_call_id = state.current.map_or(state.logical_id, |_| Uuid::now_v7());
        state.in_flight = Some(provider_call_id);
        Ok(provider_call_id)
    }

    fn publish(&self, provider_call_id: Uuid) {
        let mut state = self.lock();
        state.current = Some(provider_call_id);
        if !state.published.contains(&provider_call_id) {
            state.published.push(provider_call_id);
        }
    }

    fn release(&self, uncertain: bool) {
        let mut state = self.lock();
        state.in_flight = None;
        if uncertain {
            state.admission_uncertain = true;
        }
    }

    fn finish(&self, provider_call_id: Uuid) {
        let mut state = self.lock();
        if state.in_flight == Some(provider_call_id) {
            state.in_flight = None;
        }
    }

    fn lock(&self) -> MutexGuard<'_, ProviderSendState> {
        // A poisoned scope would silently detach rows from their replies; the
        // data is plain attribution and ids, so recovery is safe and explicit.
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

tokio::task_local! {
    static PROVIDER_SEND_SCOPE: ProviderSendScope;
}

/// Runs `future` with `scope` installed, restoring the enclosing scope on exit.
///
/// Nested logical calls therefore see their own attribution and current row,
/// and a concurrent fan-out cannot reach the enclosing call's scope.
pub async fn with_provider_send<F, T>(scope: ProviderSendScope, future: F) -> T
where
    F: Future<Output = T>,
{
    PROVIDER_SEND_SCOPE.scope(scope, future).await
}

#[must_use]
pub fn current_send_scope() -> Option<ProviderSendScope> {
    PROVIDER_SEND_SCOPE.try_with(Clone::clone).ok()
}

#[must_use]
pub fn current_send_attribution() -> Option<ProviderSendAttribution> {
    current_send_scope().map(|scope| scope.attribution())
}

#[must_use]
pub fn current_send_id() -> Option<Uuid> {
    current_send_scope().and_then(|scope| scope.current_send_id())
}

/// Claims the durable row of the next physical send of the current scope.
pub(crate) fn claim_current_send_row() -> Result<Uuid, ProviderSendClaimError> {
    match current_send_scope() {
        Some(scope) => scope.claim(),
        None => Err(ProviderSendClaimError::NoAttribution),
    }
}

/// Publishes a committed row so the caller settles the reply against it.
pub(crate) fn publish_current_send_row(provider_call_id: Uuid) {
    if let Some(scope) = current_send_scope() {
        scope.publish(provider_call_id);
    }
}

/// Releases a claim whose durable insert did not commit.
///
/// `uncertain` is `true` when the admission failed at the persistence or
/// commit-acknowledgement boundary (so the server may have committed a debit),
/// and `false` only when admission was definitively refused before commit.
pub(crate) fn release_current_send_row(uncertain: bool) {
    if let Some(scope) = current_send_scope() {
        scope.release(uncertain);
    }
}

/// Marks the transport of `provider_call_id` as finished, so the next retry may
/// claim a new row.
pub(crate) fn finish_current_send_row(provider_call_id: Uuid) {
    if let Some(scope) = current_send_scope() {
        scope.finish(provider_call_id);
    }
}

/// Test-only: publishes a row a test admitted itself.
///
/// A mocked gateway bypasses the real transport, so it must announce the row it
/// admitted for the caller's settlement exactly as `send_provider_http_request`
/// does. Compiled only with the test-support feature; never a production path.
#[cfg(feature = "test-support")]
pub fn publish_test_send_row(provider_call_id: Uuid) {
    publish_current_send_row(provider_call_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribution() -> ProviderSendAttribution {
        ProviderSendAttribution {
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            owning_execution_kind: "ingest_attempt".to_string(),
            owning_execution_id: Uuid::now_v7(),
            call_kind: "embed_chunk".to_string(),
            binding_id: None,
            provider_catalog_id: Uuid::now_v7(),
            model_catalog_id: Uuid::now_v7(),
            runtime_execution_id: None,
            runtime_task_kind: None,
        }
    }

    fn scope() -> ProviderSendScope {
        ProviderSendScope::new(attribution(), Uuid::now_v7())
    }

    #[tokio::test]
    async fn first_send_reuses_the_logical_id_and_a_retry_claims_a_new_row() {
        let scope = scope();
        let logical = scope.logical_id();
        with_provider_send(scope.clone(), async {
            let first = claim_current_send_row().expect("first claim");
            assert_eq!(first, logical);
            publish_current_send_row(first);
            finish_current_send_row(first);
        })
        .await;
        assert_eq!(scope.current_send_id(), Some(logical));

        with_provider_send(scope.clone(), async {
            let second = claim_current_send_row().expect("retry claim");
            assert_ne!(second, logical);
            publish_current_send_row(second);
        })
        .await;
        assert_ne!(scope.current_send_id(), Some(logical));
    }

    #[tokio::test]
    async fn a_second_in_flight_send_in_one_scope_is_refused() {
        let scope = scope();
        with_provider_send(scope.clone(), async {
            let first = claim_current_send_row().expect("first claim");
            assert_eq!(claim_current_send_row(), Err(ProviderSendClaimError::Busy));
            // A refused second claim must not move the current row.
            assert_eq!(scope.current_send_id(), None);
            finish_current_send_row(first);
            assert!(claim_current_send_row().is_ok());
        })
        .await;
    }

    #[tokio::test]
    async fn a_nested_scope_restores_the_enclosing_one() {
        let outer = scope();
        let outer_attribution = outer.attribution();
        with_provider_send(outer.clone(), async {
            let outer_claim = claim_current_send_row().expect("outer claim");
            let inner = scope();
            let inner_logical = inner.logical_id();
            let claimed = with_provider_send(inner, async { claim_current_send_row() }).await;
            assert_eq!(claimed.expect("inner claim"), inner_logical);
            // The enclosing scope is restored with its own in-flight claim and
            // its own attribution, never the inner call's.
            assert_eq!(current_send_attribution(), Some(outer_attribution));
            assert_eq!(claim_current_send_row(), Err(ProviderSendClaimError::Busy));
            finish_current_send_row(outer_claim);
        })
        .await;
    }

    #[tokio::test]
    async fn two_concurrent_logical_calls_never_cross_settle() {
        let first_scope = scope();
        let second_scope = scope();
        let first_spawn_scope = first_scope.clone();
        let first = tokio::spawn(with_provider_send(first_spawn_scope, async move {
            let id = claim_current_send_row().expect("first claim");
            publish_current_send_row(id);
            id
        }));
        let second_spawn_scope = second_scope.clone();
        let second = tokio::spawn(with_provider_send(second_spawn_scope, async move {
            let id = claim_current_send_row().expect("second claim");
            publish_current_send_row(id);
            id
        }));
        let first_id = first.await.expect("first task");
        let second_id = second.await.expect("second task");
        assert_ne!(first_id, second_id);
        assert_eq!(first_scope.current_send_id(), Some(first_id));
        assert_eq!(second_scope.current_send_id(), Some(second_id));
        assert_eq!(first_scope.attribution().owning_execution_kind, "ingest_attempt");
        assert_eq!(second_scope.attribution().owning_execution_kind, "ingest_attempt");
    }

    #[tokio::test]
    async fn a_scope_without_attribution_refuses_a_claim() {
        assert_eq!(claim_current_send_row(), Err(ProviderSendClaimError::NoAttribution));
    }

    #[tokio::test]
    async fn scope_tracks_all_published_sends_and_uncertain_admissions() {
        let scope = scope();
        let logical = scope.logical_id();
        assert!(!scope.has_uncertain_admission());
        assert!(scope.published_send_ids().is_empty());

        with_provider_send(scope.clone(), async {
            let first = claim_current_send_row().expect("first claim");
            // While claimed and not yet published, admission outcome is uncertain.
            assert!(scope.has_uncertain_admission());
            publish_current_send_row(first);
            assert!(!scope.has_uncertain_admission());
            finish_current_send_row(first);

            let second = claim_current_send_row().expect("retry claim");
            assert_ne!(second, logical);
            publish_current_send_row(second);
            finish_current_send_row(second);
            assert_eq!(scope.published_send_ids(), vec![first, second]);

            let third = claim_current_send_row().expect("third claim");
            assert_ne!(third, second);
            // Definitive refusal before commit clears uncertainty.
            release_current_send_row(false);
            assert!(!scope.has_uncertain_admission());

            let _fourth = claim_current_send_row().expect("fourth claim");
            // Persistence/commit-ack error latches uncertainty permanently for this scope.
            release_current_send_row(true);
            assert!(scope.has_uncertain_admission());
        })
        .await;
    }
}
