//! Reusable cancellation primitives for the IRG-203 cancellation class.
//!
//! Two distinct mistakes recur when work is moved onto a detached
//! [`tokio::spawn`] task and the caller can be dropped or aborted:
//!
//! * awaiting the spawned [`tokio::task::JoinHandle`] is *not* a cancellation
//!   link — when the caller future is dropped tokio detaches the task, which is
//!   exactly what stateful work wants ([`spawn_detached_awaited`]);
//! * owning a [`tokio::task::JoinHandle`] and dropping it is *not* aborting it —
//!   the task keeps running, which is exactly what a reporter loop must not do
//!   ([`AbortOnDrop`]).
//!
//! Both helpers name the IRG-203 class so a future reader can find the other
//! sites that share the pattern.

use std::future::Future;

/// Spawns `future` on a detached task, attaching `span`, and awaits its
/// [`tokio::task::JoinHandle`].
///
/// Awaiting the handle keeps the caller's reply shape and surfaces a panic as a
/// [`tokio::task::JoinError`]. But when the caller future is dropped — a client
/// or proxy abandoning the request, or a runtime shutting down — the awaited
/// handle is dropped too, and tokio **detaches** rather than aborts the spawned
/// task. The work therefore keeps running and reaches a terminal state instead
/// of being cancelled mid-flight. This is the IRG-199 pattern established by
/// `ContentService::materialize_inline_text_mutation` (see
/// `apps/api/src/services/content/service/pipeline/mutations.rs:61-128`), where
/// a disconnect used to kill `extract_graph` and leave the attempt `leased`.
///
/// A spawned task does not inherit the ambient span, so `span` is attached
/// explicitly: detached work must stay correlatable with the request that
/// admitted it.
pub async fn spawn_detached_awaited<F, T>(
    future: F,
    span: tracing::Span,
) -> Result<T, tokio::task::JoinError>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(tracing::Instrument::instrument(future, span)).await
}

/// Runs a synchronous `finalize` closure when dropped unless
/// [`Self::disarm`] was called first.
///
/// The IRG-199 pattern (`InlineAttemptAbortGuard`,
/// `apps/api/src/services/content/service/pipeline/mutations.rs:16-59`)
/// finalizes an attempt when a detached pipeline exits before its own
/// finalizer. This generalizes the shape for SITE-2's reset+admit sequence: if
/// the detached task unwinds — a panic, or the runtime aborting it during
/// shutdown — before admission completes, the closure schedules a best-effort
/// reconcile so a leased/accepted mutation cannot survive.
///
/// `finalize` is synchronous because a `Drop` impl cannot await; the closure
/// must schedule any async work itself via [`tokio::runtime::Handle`]. Production
/// code disarms on the success path, so the closure runs only on the abnormal
/// exit.
pub struct DetachedFinalizerGuard<F>
where
    F: FnOnce() + Send,
{
    finalize: Option<F>,
}

impl<F> DetachedFinalizerGuard<F>
where
    F: FnOnce() + Send,
{
    /// Arms the guard with the finalizer to run on drop.
    pub fn new(finalize: F) -> Self {
        Self { finalize: Some(finalize) }
    }

    /// Clears the guard, so dropping it finalizes nothing.
    pub fn disarm(&mut self) {
        self.finalize = None;
    }
}

impl<F> Drop for DetachedFinalizerGuard<F>
where
    F: FnOnce() + Send,
{
    fn drop(&mut self) {
        if let Some(finalize) = self.finalize.take() {
            tracing::warn!(
                "detached task exited before disarming its finalizer; running the abort guard"
            );
            finalize();
        }
    }
}

/// Owns a spawned task and aborts it when dropped.
///
/// A [`tokio::task::JoinHandle`] dropped on its own only detaches the task, so
/// the same IRG-203 class of bug appears when a reporter loop outlives the work
/// it reports on. This is the single guard for that shape: the former bespoke
/// `ProgressReporterGuard` (extract_graph chunk progress) and `HeartbeatGuard`
/// (maintenance lease heartbeat) both use it now, instead of three copies of
/// `impl Drop { self.0.abort() }`. [`Self::disarm`] returns the handle so a
/// caller can abort or await it explicitly on a path that must not wait for
/// drop.
pub struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    /// Wraps a spawned task's handle.
    pub fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self { handle: Some(handle) }
    }

    /// The owned handle, if the guard is still armed.
    pub fn handle_mut(&mut self) -> Option<&mut tokio::task::JoinHandle<T>> {
        self.handle.as_mut()
    }

    /// Consumes the guard, returning the handle without aborting it.
    ///
    /// The returned handle detaches the task when dropped; the caller decides
    /// whether to abort or await it.
    pub fn take(mut self) -> Option<tokio::task::JoinHandle<T>> {
        self.handle.take()
    }

    /// Clears the guard and returns the handle, so drop no longer aborts.
    ///
    /// Used on the normal path when the caller wants to abort or await the task
    /// explicitly and have the drop guard stay silent afterwards.
    pub fn disarm(&mut self) -> Option<tokio::task::JoinHandle<T>> {
        self.handle.take()
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// One-shot test seam for the IRG-203 detached-work windows.
///
/// Two paths deliberately run durable state-mutating work on a detached task so
/// a caller abort cannot cancel it mid-way, and both must be provably reachable
/// in that window:
///
/// * SITE-4 detaches an ordered bulk action and a concurrent batch cancel; the
///   fan-out pauses after its first item, so a test can abort the caller and
///   assert every remaining item still applied.
/// * SITE-6 writes the content-addressed source blob *before* the admission
///   transaction commits a revision that references it; the upload pauses
///   between the write and admission, so a test can abort the caller and assert
///   the compensating [`PersistedRevisionSourceGuard`] reclaims the orphan.
///
/// One primitive serves both — a bespoke copy per site was the same code twice.
/// A test installs a probe; the parked path signals `reached` and waits for
/// `release` (or the test's abort). It is a no-op with no probe installed, and
/// the whole module compiles out of production builds.
///
/// [`PersistedRevisionSourceGuard`]: crate::services::content::service::PersistedRevisionSourceGuard
#[cfg(feature = "test-support")]
pub mod one_shot_pause_probe {
    use std::sync::{Mutex, OnceLock};

    use tokio::sync::oneshot;

    struct PendingProbe {
        reached_tx: oneshot::Sender<()>,
        release_rx: oneshot::Receiver<()>,
    }

    fn pending() -> &'static Mutex<Option<PendingProbe>> {
        static PENDING: OnceLock<Mutex<Option<PendingProbe>>> = OnceLock::new();
        PENDING.get_or_init(|| Mutex::new(None))
    }

    /// Handle for a test driving an installed probe. `reached` fires once the
    /// parked path has applied its durable step; send on `release` (or drop it)
    /// to let it continue, or abort the caller instead.
    pub struct PauseProbeHandle {
        pub reached: oneshot::Receiver<()>,
        pub release: oneshot::Sender<()>,
    }

    /// Installs a one-shot probe for the next parked path.
    pub fn install() -> PauseProbeHandle {
        let (reached_tx, reached) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        *pending().lock().expect("pause probe mutex poisoned") =
            Some(PendingProbe { reached_tx, release_rx });
        PauseProbeHandle { reached, release }
    }

    /// Signals the installed probe and parks until released; a no-op without one.
    pub async fn pause() {
        let probe = pending().lock().expect("pause probe mutex poisoned").take();
        if let Some(PendingProbe { reached_tx, release_rx }) = probe {
            let _ = reached_tx.send(());
            let _ = release_rx.await;
        }
    }
}

/// One-shot test seam for the IRG-218 graph-convergence step of a batch delete.
///
/// The batch delete deletes its documents first and then converges the library
/// graph in the background. A projection failure there must not be written as
/// the parent's status (the children decide it); a DB test arms this fault to
/// drive that branch deterministically and assert the parent is still `ready`
/// with the failure recorded as a detail. One-shot: the next `take` disarms it.
/// Compiled out of production builds.
#[cfg(feature = "test-support")]
pub mod graph_convergence_fault {
    use std::sync::atomic::{AtomicBool, Ordering};

    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Arms the fault for the next graph-projection refresh.
    pub fn install() {
        ARMED.store(true, Ordering::SeqCst);
    }

    /// Consumes the armed fault, returning `true` when this call must fail.
    pub fn take() -> bool {
        ARMED.swap(false, Ordering::SeqCst)
    }
}

/// One-shot test seam for the inline revision-chunk vector-delete step of a
/// document delete.
///
/// A document delete commits first and then, best-effort, deletes each
/// revision's chunks and vectors. A refusal there must stay a loud structured
/// `tracing::error!` while the accepted delete still returns success, and the
/// periodic `gc.superseded-vectors` sweep must remain the durable backstop that
/// reclaims the vectors. A DB test arms this fault by revision id to drive that
/// branch deterministically without a production `DELETE` or an unrelated
/// mutation; it is keyed by revision so parallel suites cannot consume it.
/// One-shot: the next `take` for the same revision id disarms it. Compiled out
/// of production builds.
#[cfg(feature = "test-support")]
pub mod revision_chunk_vector_delete_fault {
    use std::sync::Mutex;

    use uuid::Uuid;

    static ARMED: Mutex<Vec<Uuid>> = Mutex::new(Vec::new());

    /// Arms the fault for the next vector delete of `revision_id`.
    pub fn install(revision_id: Uuid) {
        ARMED.lock().expect("revision chunk vector delete fault mutex poisoned").push(revision_id);
    }

    /// Consumes an armed fault for `revision_id`, returning `true` when this
    /// call must be refused.
    pub fn take(revision_id: Uuid) -> bool {
        let mut armed = ARMED.lock().expect("revision chunk vector delete fault mutex poisoned");
        if let Some(index) = armed.iter().position(|candidate| *candidate == revision_id) {
            armed.remove(index);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn abort_on_drop_aborts_its_task() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_task = Arc::clone(&ran);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            ran_in_task.store(true, Ordering::SeqCst);
        });

        let guard = AbortOnDrop::new(handle);
        // Let the task register its sleep timer before the drop, so the clock
        // advance below has a live timer to fire if the guard detaches instead
        // of aborting.
        tokio::task::yield_now().await;
        drop(guard);

        tokio::time::advance(Duration::from_secs(7200)).await;
        tokio::task::yield_now().await;

        assert!(
            !ran.load(Ordering::SeqCst),
            "dropping AbortOnDrop must abort its task before the delayed work runs",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn spawn_detached_awaited_completes_after_caller_is_dropped() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u32>(1);
        let inner_started = Arc::new(AtomicBool::new(false));
        let inner_started_in_task = Arc::clone(&inner_started);

        let caller = tokio::spawn(async move {
            spawn_detached_awaited(
                async move {
                    inner_started_in_task.store(true, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    let _ = tx.send(42).await;
                },
                tracing::Span::current(),
            )
            .await
        });

        for _ in 0..64 {
            if inner_started.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            inner_started.load(Ordering::SeqCst),
            "the awaited spawned task must have started before the caller is aborted",
        );

        caller.abort();
        let _ = caller.await;

        tokio::time::advance(Duration::from_secs(20)).await;
        let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("detached work must not be cancelled with its caller")
            .expect("the detached task must still send its value");
        assert_eq!(
            received, 42,
            "aborting the caller must detach, not cancel, the awaited spawned work",
        );
    }

    #[test]
    fn detached_finalizer_guard_runs_on_drop_and_is_silent_after_disarm() {
        use std::sync::atomic::AtomicUsize;
        let ran = Arc::new(AtomicUsize::new(0));

        {
            let _guard = DetachedFinalizerGuard::new({
                let ran = Arc::clone(&ran);
                move || {
                    ran.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "an armed guard must run its finalizer when the task scope ends without disarming",
        );

        let mut guard = DetachedFinalizerGuard::new({
            let ran = Arc::clone(&ran);
            move || {
                ran.fetch_add(1, Ordering::SeqCst);
            }
        });
        guard.disarm();
        drop(guard);
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "a disarmed guard must not finalize on the success path",
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test(start_paused = true)]
    async fn one_shot_pause_probe_pauses_until_released() {
        let probe = one_shot_pause_probe::install();
        let caller = tokio::spawn(async move {
            one_shot_pause_probe::pause().await;
        });

        tokio::time::timeout(Duration::from_secs(1), probe.reached)
            .await
            .expect("the fan-out must reach the probe after its first item")
            .expect("the probe sender must fire");

        // The paused fan-out must not pass the probe until the test releases it.
        tokio::task::yield_now().await;
        assert!(!caller.is_finished(), "the probe must hold the fan-out until released");

        let _ = probe.release.send(());
        tokio::time::timeout(Duration::from_secs(1), caller)
            .await
            .expect("releasing the probe must let the fan-out continue")
            .expect("the paused fan-out task must not panic");
    }
}
