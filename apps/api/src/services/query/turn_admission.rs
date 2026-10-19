//! Bounded query-turn admission so a burst cannot open unbounded Postgres work.
//!
//! The permit covers one outermost retrieval turn. Nested grounded-answer
//! calls inherit the parent permit through a task-local, so the assistant
//! agent loop cannot deadlock on itself. Excess waiters are rejected with a
//! retryable 503 instead of sitting on the pool until the 180s turn budget.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::time::{Instant, timeout};

use crate::interfaces::http::router_support::ApiError;
use crate::shared::admission::WaitingSlot;

tokio::task_local! {
    static HOLDING_TURN_ADMISSION: ();
}

#[derive(Clone, Debug)]
pub(crate) struct QueryTurnAdmissionConfig {
    pub max_concurrent: usize,
    pub max_queue: usize,
    pub acquire_timeout: Duration,
}

impl QueryTurnAdmissionConfig {
    pub(crate) fn from_settings(
        max_concurrent: usize,
        max_queue: usize,
        acquire_timeout_ms: u64,
    ) -> Result<Self, String> {
        if max_concurrent == 0 {
            return Err("query_turn_max_concurrent must be greater than zero".into());
        }
        if acquire_timeout_ms == 0 {
            return Err("query_turn_admission_timeout_ms must be greater than zero".into());
        }
        Ok(Self {
            max_concurrent,
            max_queue,
            acquire_timeout: Duration::from_millis(acquire_timeout_ms),
        })
    }
}

#[derive(Debug)]
pub(crate) struct QueryTurnAdmission {
    semaphore: Arc<Semaphore>,
    waiting: Arc<AtomicUsize>,
    config: QueryTurnAdmissionConfig,
}

impl Clone for QueryTurnAdmission {
    fn clone(&self) -> Self {
        Self {
            semaphore: Arc::clone(&self.semaphore),
            waiting: Arc::clone(&self.waiting),
            config: self.config.clone(),
        }
    }
}

/// Records acquisition only, including cancellation. The permit's lifetime and
/// the answer pipeline must not be attributed to admission wait.
struct AdmissionObservation {
    span: tracing::Span,
    started: Instant,
    outcome: &'static str,
    queued: bool,
}

impl AdmissionObservation {
    fn new() -> Self {
        Self {
            span: tracing::Span::current(),
            started: Instant::now(),
            outcome: "cancelled",
            queued: false,
        }
    }
}

impl Drop for AdmissionObservation {
    fn drop(&mut self) {
        let elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        self.span.record("admission.outcome", self.outcome);
        self.span.record("admission.queued", self.queued);
        self.span.record("admission.wait_ms", elapsed_ms);
        tracing::info!(
            parent: &self.span,
            stage = "query.admission.completed",
            outcome = self.outcome,
            queued = self.queued,
            elapsed_ms,
            "query turn admission completed"
        );
    }
}

impl QueryTurnAdmission {
    #[must_use]
    pub(crate) fn new(config: QueryTurnAdmissionConfig) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
            waiting: Arc::new(AtomicUsize::new(0)),
            config,
        }
    }

    #[tracing::instrument(
        name = "query.turn.admission",
        skip_all,
        fields(
            admission.outcome = tracing::field::Empty,
            admission.queued = tracing::field::Empty,
            admission.wait_ms = tracing::field::Empty
        )
    )]
    pub(crate) async fn acquire(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        let mut observation = AdmissionObservation::new();
        // A free permit needs no queue slot, so take it without touching the
        // queue counter. Only a caller that has to wait reserves a slot, and it
        // reserves it atomically with the bound check so a simultaneous burst
        // cannot all observe room and then overflow `max_queue`.
        match Arc::clone(&self.semaphore).try_acquire_owned() {
            Ok(permit) => {
                observation.outcome = "admitted";
                return Ok(permit);
            }
            Err(TryAcquireError::NoPermits) => {}
            Err(TryAcquireError::Closed) => {
                observation.outcome = "closed";
                return Err(admission_rejected());
            }
        }
        let Some(_waiting) = WaitingSlot::try_reserve(self.waiting.as_ref(), self.config.max_queue)
        else {
            observation.outcome = "queue_full";
            return Err(admission_rejected());
        };
        observation.queued = true;
        let acquired =
            timeout(self.config.acquire_timeout, Arc::clone(&self.semaphore).acquire_owned()).await;
        match acquired {
            Ok(Ok(permit)) => {
                observation.outcome = "admitted";
                Ok(permit)
            }
            Ok(Err(_)) => {
                observation.outcome = "closed";
                Err(admission_rejected())
            }
            Err(_) => {
                observation.outcome = "timeout";
                Err(admission_rejected())
            }
        }
    }

    #[cfg(test)]
    fn queued_waiters(&self) -> usize {
        self.waiting.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn admission_rejected() -> ApiError {
    ApiError::service_unavailable(
        "query turn admission queue is full; retry the request",
        "query_admission_rejected",
    )
}

pub(crate) async fn with_query_turn_admission<F, T>(
    admission: QueryTurnAdmission,
    future: F,
) -> Result<T, ApiError>
where
    F: std::future::Future<Output = Result<T, ApiError>>,
{
    if HOLDING_TURN_ADMISSION.try_with(|_| ()).is_ok() {
        return future.await;
    }
    let permit = admission.acquire().await?;
    HOLDING_TURN_ADMISSION
        .scope((), async move {
            let result = future.await;
            drop(permit);
            result
        })
        .await
}

#[cfg(test)]
mod observation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn production_default_admits_a_hundred_turn_burst_into_the_queue() {
        let config = QueryTurnAdmissionConfig::from_settings(32, 80, 15_000)
            .expect("production defaults must validate");
        assert!(config.max_concurrent + config.max_queue >= 100);
        assert!(config.acquire_timeout < Duration::from_secs(180));
    }

    #[tokio::test]
    async fn nested_admission_reuses_the_parent_permit() {
        let admission = QueryTurnAdmission::new(QueryTurnAdmissionConfig {
            max_concurrent: 1,
            max_queue: 0,
            acquire_timeout: Duration::from_millis(20),
        });
        let outcome = with_query_turn_admission(admission.clone(), async {
            with_query_turn_admission(admission, async { Ok::<_, ApiError>(7) }).await
        })
        .await
        .expect("nested admission should not deadlock");
        assert_eq!(outcome, 7);
    }

    #[tokio::test]
    async fn saturated_queue_rejects_immediately() {
        let admission = QueryTurnAdmission::new(QueryTurnAdmissionConfig {
            max_concurrent: 1,
            max_queue: 0,
            acquire_timeout: Duration::from_secs(5),
        });
        let _permit = admission.acquire().await.expect("first permit");
        let error = admission.acquire().await.expect_err("queue is full");
        assert_eq!(error.kind(), "query_admission_rejected");
    }

    /// Waiters cancelled in each round. The queue bound equals this count, so a
    /// leaked counter makes the next admission look full while nothing waits.
    const CANCELLED_WAITERS: usize = 4;

    /// Polls `acquire` once so the waiter registers, then drops the future from
    /// the outer deadline: the cancellation path a disconnecting client takes.
    async fn cancel_one_acquire(admission: &QueryTurnAdmission) {
        let cancelled = tokio::time::timeout(Duration::from_millis(20), admission.acquire()).await;
        assert!(cancelled.is_err(), "the outer deadline must cancel the pending waiter");
    }

    #[tokio::test]
    async fn cancelled_waiters_release_the_waiting_counter() {
        let admission = QueryTurnAdmission::new(QueryTurnAdmissionConfig {
            max_concurrent: 1,
            max_queue: CANCELLED_WAITERS,
            acquire_timeout: Duration::from_secs(5),
        });
        let held = admission.acquire().await.expect("first permit");

        for _ in 0..CANCELLED_WAITERS {
            cancel_one_acquire(&admission).await;
        }

        assert_eq!(
            admission.waiting.load(Ordering::SeqCst),
            0,
            "cancelled waiters must release the waiting counter"
        );
        drop(held);
    }

    #[tokio::test]
    async fn cancelled_waiters_do_not_reject_an_empty_queue() {
        let admission = QueryTurnAdmission::new(QueryTurnAdmissionConfig {
            max_concurrent: 1,
            max_queue: CANCELLED_WAITERS,
            acquire_timeout: Duration::from_secs(5),
        });
        let held = admission.acquire().await.expect("first permit");

        for _ in 0..CANCELLED_WAITERS {
            cancel_one_acquire(&admission).await;
        }

        // Nothing waits now, so a new turn queues behind the held permit and
        // only the outer deadline ends it. An immediate rejection means the
        // queue was judged full by a leaked counter.
        let outcome = tokio::time::timeout(Duration::from_millis(50), admission.acquire()).await;
        assert!(
            outcome.is_err(),
            "an empty queue rejected admission; the waiter was not cancelled by the deadline"
        );
        drop(held);
    }

    /// The wait queue must stay within `max_queue` when callers arrive at the
    /// same instant. A bound check followed by a separate increment lets a
    /// simultaneous burst all observe room and then overflow the queue.
    ///
    /// Each caller is a real OS thread released together by a
    /// `std::sync::Barrier`: only then does each thread run its own `acquire`
    /// poll at the same moment, which is what puts several callers between the
    /// bound check and the counter increment. An async-only barrier schedules
    /// the polls through the runtime and serialises enough of them to hide the
    /// race, which is why that shape passed on the broken code.
    #[test]
    fn concurrent_waiters_never_overflow_the_queue_bound() {
        use std::sync::Barrier;

        const QUEUE_BOUND: usize = 1;
        const BURST: usize = 64;
        const ROUNDS: usize = 32;

        for round in 0..ROUNDS {
            let admission = Arc::new(QueryTurnAdmission::new(QueryTurnAdmissionConfig {
                max_concurrent: 1,
                max_queue: QUEUE_BOUND,
                acquire_timeout: Duration::from_millis(500),
            }));

            // The permit is owned by the admission, not by the runtime that
            // took it, so a throwaway runtime is enough to hold it.
            let held = {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("runtime");
                runtime.block_on(admission.acquire()).expect("sole permit must be admitted")
            };

            // Release every caller together. A per-caller delay would serialise
            // the admission check and hide the race. Each caller gets its own
            // runtime, built before the gate: a shared `Handle::block_on` does
            // not let many external threads drive many futures at once.
            let gate = Arc::new(Barrier::new(BURST + 1));
            let rejected = Arc::new(AtomicUsize::new(0));
            let mut callers = Vec::with_capacity(BURST);
            for _ in 0..BURST {
                let admission = Arc::clone(&admission);
                let gate = Arc::clone(&gate);
                let rejected = Arc::clone(&rejected);
                callers.push(std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .expect("caller runtime");
                    gate.wait();
                    if runtime.block_on(admission.acquire()).is_err() {
                        rejected.fetch_add(1, Ordering::SeqCst);
                    }
                }));
            }
            gate.wait();

            // A queued caller holds a queue slot while it waits for a turn; a
            // rejected one adds to `rejected`. Once the two add up to the
            // burst, every caller has taken its decision. Sample the queue
            // before releasing the permit, while the waiters still hold it.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut decided = 0;
            while std::time::Instant::now() < deadline {
                decided = admission.queued_waiters() + rejected.load(Ordering::SeqCst);
                if decided >= BURST {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let queued = admission.queued_waiters();

            drop(held);
            for caller in callers {
                caller.join().expect("caller thread must not panic");
            }

            assert_eq!(decided, BURST, "round {round}: not every caller decided");
            assert!(
                queued <= QUEUE_BOUND,
                "round {round}: {queued} callers entered a wait queue bounded at {QUEUE_BOUND}",
            );
        }
    }
}
