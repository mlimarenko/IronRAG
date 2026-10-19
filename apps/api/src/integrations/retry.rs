use super::duration_millis;
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tracing::Instrument as _;

use crate::{
    integrations::provider_budget::{
        ProviderBudgetError, ProviderBudgetGuard, ProviderBudgetRegistry, ProviderIdentity,
        current_lane,
    },
    shared::outbound_http::PublicHttpUrlError,
};
use reqwest::{
    StatusCode,
    header::{HeaderMap, RETRY_AFTER},
};

mod provider;

pub use provider::{
    ProviderCallError, ProviderRateLimit, provider_http_status_error,
    provider_http_status_error_unread, provider_status_error_from_read,
    sanitize_provider_error_detail,
};
use provider::{abort_retry, classify_provider_call_error, retry_delay};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry,
    /// Retry this failure at most once per whole call, independently of the
    /// remaining attempt budget. Used for a proven-empty upstream 404: the
    /// first blank retries, a second one aborts even when the policy would
    /// otherwise allow more attempts.
    RetryOnce,
    Abort,
    RetryAfter(Duration),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderRetryTelemetry {
    pub provider_429_count: u64,
    pub provider_retry_count: u64,
    pub degraded_completion_count: u64,
}

#[derive(Debug, Default)]
struct ProviderRetryMetrics {
    provider_429_count: AtomicU64,
    provider_retry_count: AtomicU64,
    degraded_completion_count: AtomicU64,
}

impl ProviderRetryMetrics {
    fn snapshot(&self) -> ProviderRetryTelemetry {
        ProviderRetryTelemetry {
            provider_429_count: self.provider_429_count.load(Ordering::Relaxed),
            provider_retry_count: self.provider_retry_count.load(Ordering::Relaxed),
            degraded_completion_count: self.degraded_completion_count.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
struct ProviderRetryExecution {
    deadline: tokio::time::Instant,
    metrics: Arc<ProviderRetryMetrics>,
}

tokio::task_local! {
    static PROVIDER_RETRY_EXECUTION: ProviderRetryExecution;
}

/// Runs provider work under the caller's remaining execution budget.
///
/// Nested scopes retain the execution's counters and narrow, but never extend,
/// its absolute deadline. Provider retries therefore follow the nearest real
/// pipeline boundary while remaining attributable to one execution artifact.
pub async fn with_provider_retry_execution<F, T>(deadline: Duration, future: F) -> T
where
    F: Future<Output = T>,
{
    let requested_deadline = tokio::time::Instant::now() + deadline;
    let execution = PROVIDER_RETRY_EXECUTION
        .try_with(|current| ProviderRetryExecution {
            deadline: current.deadline.min(requested_deadline),
            metrics: Arc::clone(&current.metrics),
        })
        .unwrap_or_else(|_| ProviderRetryExecution {
            deadline: requested_deadline,
            metrics: Arc::new(ProviderRetryMetrics::default()),
        });
    PROVIDER_RETRY_EXECUTION.scope(execution, future).await
}

#[must_use]
pub fn current_provider_retry_telemetry() -> ProviderRetryTelemetry {
    PROVIDER_RETRY_EXECUTION.try_with(|execution| execution.metrics.snapshot()).unwrap_or_default()
}

pub fn record_degraded_completion() {
    let _ = PROVIDER_RETRY_EXECUTION.try_with(|execution| {
        execution.metrics.degraded_completion_count.fetch_add(1, Ordering::Relaxed);
    });
}

fn record_provider_429() {
    // The typed 429 never reaches a billing capture point (only `failed`/
    // `canceled` survive upstream), and this layer has neither call_kind nor
    // model, so the rate limit is counted here with `unknown` labels (IRG-204).
    crate::observability::metrics::record_provider_call(
        "unknown",
        "unknown",
        crate::observability::metrics::ProviderCallStatus::RateLimited,
    );
    let _ = PROVIDER_RETRY_EXECUTION.try_with(|execution| {
        execution.metrics.provider_429_count.fetch_add(1, Ordering::Relaxed);
    });
}

fn record_provider_retry() {
    let _ = PROVIDER_RETRY_EXECUTION.try_with(|execution| {
        execution.metrics.provider_retry_count.fetch_add(1, Ordering::Relaxed);
    });
}

fn current_provider_retry_deadline() -> Option<tokio::time::Instant> {
    PROVIDER_RETRY_EXECUTION.try_with(|execution| execution.deadline).ok()
}

#[derive(Debug)]
pub struct RetryPolicy<E = ProviderCallError> {
    max_attempts: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    jitter: bool,
    classify: fn(&E) -> RetryDecision,
}

impl<E> Copy for RetryPolicy<E> {}

impl<E> Clone for RetryPolicy<E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E> RetryPolicy<E> {
    #[must_use]
    const fn new(
        max_attempts: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
        jitter: bool,
        classify: fn(&E) -> RetryDecision,
    ) -> Self {
        Self { max_attempts, initial_backoff, max_backoff, jitter, classify }
    }

    #[must_use]
    pub const fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            jitter: false,
            classify: abort_retry,
        }
    }
}

impl RetryPolicy<ProviderCallError> {
    /// Builds a retry policy for provider operations whose repeated execution
    /// has no externally visible mutation.
    #[must_use]
    pub const fn idempotent_provider_call(
        max_attempts: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        Self::new(max_attempts, initial_backoff, max_backoff, true, classify_provider_call_error)
    }
}

pub async fn with_retry<F, Fut, T, E>(mut call: F, policy: RetryPolicy<E>) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt = 1_u32;
    // Tracks whether the single whole-call retry reserved for a
    // `RetryDecision::RetryOnce` class has already been spent.
    let mut once_retry_used = false;

    loop {
        match call().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let (decision, used) =
                    apply_single_retry_budget((policy.classify)(&error), once_retry_used);
                once_retry_used = used;
                if matches!(decision, RetryDecision::Abort) {
                    return Err(error);
                }
                let deadline = current_provider_retry_deadline();
                if deadline.is_none() && attempt >= max_attempts {
                    return Err(error);
                }
                let delay = retry_delay_for_decision(&policy, attempt, decision);
                if deadline.is_some_and(|deadline| {
                    delay >= deadline.saturating_duration_since(tokio::time::Instant::now())
                }) {
                    return Err(error);
                }
                let retry_after_honored = matches!(decision, RetryDecision::RetryAfter(_));

                let span = tracing::info_span!(
                    "provider.retry_wait",
                    attempt,
                    next_attempt = attempt.saturating_add(1),
                    max_attempts,
                    deadline_bound = deadline.is_some(),
                    delay_ms = duration_millis(delay),
                    retry_after_honored,
                );
                tokio::time::sleep(delay).instrument(span).await;
                record_provider_retry();
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

fn apply_single_retry_budget(decision: RetryDecision, used: bool) -> (RetryDecision, bool) {
    match (decision, used) {
        (RetryDecision::RetryOnce, false) => (RetryDecision::Retry, true),
        (RetryDecision::RetryOnce, true) => (RetryDecision::Abort, true),
        _ => (decision, used),
    }
}

/// Takes one provider attempt permit after reserving a rate token.
///
/// Every provider integration that guards an outbound attempt shares this one
/// implementation so the throttle/acquire/commit ordering cannot diverge between
/// the LLM gateway and the SystemOne client. The rate reservation is RAII: it is
/// committed only once the concurrency permit is held, so a rejected admission —
/// or an execution deadline that drops this future mid-acquire — refunds the
/// token synchronously instead of leaking it.
pub async fn acquire_provider_budget(
    registry: &ProviderBudgetRegistry,
    identity: &ProviderIdentity,
) -> Result<ProviderBudgetGuard, ProviderCallError> {
    let rate_token = registry.throttle(identity).await.map_err(|error| {
        ProviderCallError::protocol(format!("provider request throttling failed: {error}"))
    })?;
    match registry.acquire(identity, current_lane()).await {
        Ok(guard) => {
            // The attempt now holds its concurrency permit, so the token is spent
            // rather than provisional and must not be refunded.
            rate_token.commit();
            Ok(guard)
        }
        // Dropping the reservation returns the token. Reaching this arm by a
        // rejected admission and reaching it by the future being dropped while
        // the concurrency acquire is pending are the same refund path.
        Err(admission_error) => Err(ProviderCallError::admission(admission_error)),
    }
}

fn retry_delay_for_decision<E>(
    policy: &RetryPolicy<E>,
    attempt: u32,
    decision: RetryDecision,
) -> Duration {
    match decision {
        RetryDecision::Retry | RetryDecision::RetryOnce => retry_delay(policy, attempt),
        RetryDecision::RetryAfter(delay) => delay,
        RetryDecision::Abort => Duration::ZERO,
    }
}
