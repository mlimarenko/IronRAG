//! Bounded admission for shared in-process semaphores.
//!
//! A bare `Semaphore::acquire_owned().await` has no upper bound: when every
//! permit is held by a stuck or long-running job the waiter hangs forever, with
//! no metric, no log, and no chance for its own cancellation path to run. This
//! helper gives every limiter the same bounded, observable wait — a typed
//! refusal when the bound elapses, an `admission.permit_wait` span carrying the
//! measured wait, and a value-free refusal counter labelled by limiter name.

use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use opentelemetry::{KeyValue, global, metrics::Counter};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::Instrument as _;

/// Value-free admission refusal counters. Labels are the structural limiter
/// name and the refusal reason; no request, tenant or timing value reaches a
/// label.
struct AdmissionMetrics {
    permit_wait_refusals: Counter<u64>,
}

fn admission_metrics() -> &'static AdmissionMetrics {
    static METRICS: OnceLock<AdmissionMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("ironrag.admission");
        AdmissionMetrics {
            permit_wait_refusals: meter
                .u64_counter("ironrag.admission.permit_wait_refusals")
                .with_description("Shared permit waits that ended in a typed refusal")
                .with_unit("{refusal}")
                .build(),
        }
    })
}

/// Why a bounded permit wait did not produce a permit.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PermitWaitError {
    /// No permit became available within the configured bound.
    #[error("permit wait for {limiter} timed out after {timeout_ms} ms")]
    AcquireTimeout { limiter: &'static str, timeout_ms: u64 },
    /// The limiter was closed while waiting; fail closed instead of bypassing it.
    #[error("permit limiter {limiter} is closed")]
    LimiterClosed { limiter: &'static str },
}

impl PermitWaitError {
    /// The structural limiter name, safe for logs and metric labels.
    #[must_use]
    pub const fn limiter(&self) -> &'static str {
        match self {
            Self::AcquireTimeout { limiter, .. } | Self::LimiterClosed { limiter } => limiter,
        }
    }
}

/// Waits at most `timeout` for one permit from `semaphore`.
///
/// On timeout the wait is abandoned, the refusal counter is incremented for
/// `limiter` with `reason="acquire_timeout"`, and a warning records the bound
/// and the measured wait. A closed limiter is a distinct fail-closed refusal.
pub async fn acquire_permit_within(
    semaphore: Arc<Semaphore>,
    timeout: Duration,
    limiter: &'static str,
) -> Result<OwnedSemaphorePermit, PermitWaitError> {
    let span =
        tracing::info_span!("admission.permit_wait", limiter, wait_ms = tracing::field::Empty,);
    async {
        let started = Instant::now();
        let result = tokio::time::timeout(timeout, semaphore.acquire_owned()).await;
        let wait_ms = duration_millis(started.elapsed());
        tracing::Span::current().record("wait_ms", wait_ms);
        match result {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(PermitWaitError::LimiterClosed { limiter }),
            Err(_) => {
                let timeout_ms = duration_millis(timeout);
                admission_metrics().permit_wait_refusals.add(
                    1,
                    &[
                        KeyValue::new("limiter", limiter),
                        KeyValue::new("reason", "acquire_timeout"),
                    ],
                );
                tracing::warn!(limiter, timeout_ms, wait_ms, "permit wait timed out");
                Err(PermitWaitError::AcquireTimeout { limiter, timeout_ms })
            }
        }
    }
    .instrument(span)
    .await
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Reserves one slot in a bounded wait queue and releases it on drop.
///
/// The reservation is one `fetch_update`, so concurrent callers cannot all
/// observe room and then overflow the bound the way a separate check followed
/// by an increment does. The guard lives across the acquisition await, so a
/// waiter cancelled at its timeout, or panicking, releases its slot on the
/// same path a completed one does and the counter cannot leak.
#[derive(Debug)]
pub(crate) struct WaitingSlot<'a> {
    waiting: &'a AtomicUsize,
}

impl<'a> WaitingSlot<'a> {
    /// Reserves a slot when fewer than `max_queue` are already held.
    pub(crate) fn try_reserve(waiting: &'a AtomicUsize, max_queue: usize) -> Option<Self> {
        waiting
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |queued| {
                (queued < max_queue).then_some(queued + 1)
            })
            .ok()?;
        Some(Self { waiting })
    }
}

impl Drop for WaitingSlot<'_> {
    fn drop(&mut self) {
        self.waiting.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saturated_limiter_returns_a_typed_timeout() {
        let semaphore = Arc::new(Semaphore::new(1));
        let _held = Arc::clone(&semaphore).acquire_owned().await.expect("first permit");
        let started = Instant::now();

        // RED before the timeout: the wait ignored the bound and stayed
        // pending, so this outer bound elapsed. GREEN after: the helper itself
        // returns a typed refusal.
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            acquire_permit_within(semaphore, Duration::from_millis(20), "test_limiter"),
        )
        .await
        .expect("bounded wait must return instead of hanging")
        .expect_err("a saturated limiter must refuse");

        assert_eq!(
            error,
            PermitWaitError::AcquireTimeout { limiter: "test_limiter", timeout_ms: 20 }
        );
        assert!(started.elapsed() >= Duration::from_millis(20), "the bound must actually elapse");
    }

    #[tokio::test]
    async fn closed_limiter_is_a_distinct_typed_refusal() {
        let semaphore = Arc::new(Semaphore::new(1));
        semaphore.close();

        let error = acquire_permit_within(semaphore, Duration::from_secs(1), "closed_limiter")
            .await
            .expect_err("a closed limiter must refuse");

        assert_eq!(error, PermitWaitError::LimiterClosed { limiter: "closed_limiter" });
    }

    #[tokio::test]
    async fn released_capacity_is_acquired_within_the_bound() {
        let semaphore = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&semaphore).acquire_owned().await.expect("first permit");
        let mut waiting = Box::pin(acquire_permit_within(
            Arc::clone(&semaphore),
            Duration::from_secs(5),
            "released",
        ));

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting).await.is_err(),
            "the waiter must queue while the only permit is held",
        );

        drop(held);
        let _permit = waiting.await.expect("released capacity must be acquired within the bound");
    }

    #[test]
    fn waiting_slot_reserves_within_the_bound_and_refuses_beyond_it() {
        let waiting = AtomicUsize::new(0);
        let first = WaitingSlot::try_reserve(&waiting, 1).expect("the bound admits one slot");
        assert!(
            WaitingSlot::try_reserve(&waiting, 1).is_none(),
            "a second reservation must be refused while the bound is full"
        );
        drop(first);
        let _second = WaitingSlot::try_reserve(&waiting, 1).expect("the released slot is reusable");
    }

    #[test]
    fn waiting_slot_releases_its_slot_on_drop() {
        let waiting = AtomicUsize::new(0);
        {
            let _slot = WaitingSlot::try_reserve(&waiting, 2).expect("first slot");
            assert_eq!(waiting.load(Ordering::SeqCst), 1);
        }
        assert_eq!(
            waiting.load(Ordering::SeqCst),
            0,
            "dropping the guard must release the slot so a cancelled waiter cannot leak it"
        );
    }

    /// The bound check and the increment are one atomic operation. A burst of
    /// real OS threads released together must never push the counter past
    /// `max_queue`; a check followed by a separate increment does.
    #[test]
    fn concurrent_reservations_never_exceed_the_bound() {
        use std::sync::Barrier;
        use std::thread;

        const QUEUE_BOUND: usize = 1;
        const BURST: usize = 64;
        const ROUNDS: usize = 32;

        for round in 0..ROUNDS {
            let waiting = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let gate = Arc::new(Barrier::new(BURST + 1));
            let mut holders = Vec::with_capacity(BURST);
            for _ in 0..BURST {
                let waiting = Arc::clone(&waiting);
                let peak = Arc::clone(&peak);
                let gate = Arc::clone(&gate);
                holders.push(thread::spawn(move || {
                    gate.wait();
                    if let Some(slot) = WaitingSlot::try_reserve(&waiting, QUEUE_BOUND) {
                        peak.fetch_max(waiting.load(Ordering::SeqCst), Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(1));
                        drop(slot);
                    }
                }));
            }
            gate.wait();
            for holder in holders {
                holder.join().expect("reservation thread must not panic");
            }
            let observed = peak.load(Ordering::SeqCst);
            assert!(
                observed <= QUEUE_BOUND,
                "round {round}: {observed} reservations passed a queue bounded at {QUEUE_BOUND}",
            );
        }
    }
}
