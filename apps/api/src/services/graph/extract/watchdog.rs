use std::{future::Future, time::Duration};

use tokio::sync::watch;

#[derive(Clone, Copy, Default)]
struct GraphExtractionActivityState {
    active_provider_operations: usize,
    completed_chunks: u64,
}

#[derive(Clone)]
pub(crate) struct GraphExtractionActivity {
    updates: watch::Sender<GraphExtractionActivityState>,
}

pub(crate) struct GraphExtractionIdleWatchdog {
    idle_timeout: Duration,
    updates: watch::Receiver<GraphExtractionActivityState>,
}

pub(crate) struct GraphExtractionProviderOperation {
    activity: GraphExtractionActivity,
}

tokio::task_local! {
    static CURRENT_GRAPH_EXTRACTION_ACTIVITY: GraphExtractionActivity;
}

impl GraphExtractionActivity {
    pub(crate) async fn scope<F>(self, future: F) -> F::Output
    where
        F: Future,
    {
        CURRENT_GRAPH_EXTRACTION_ACTIVITY.scope(self, future).await
    }

    pub(crate) fn begin_current_provider_operation() -> Option<GraphExtractionProviderOperation> {
        CURRENT_GRAPH_EXTRACTION_ACTIVITY.try_with(Self::begin_provider_operation).ok()
    }

    pub(crate) fn record_current_chunk_completion() {
        let _ = CURRENT_GRAPH_EXTRACTION_ACTIVITY.try_with(Self::record_chunk_completion);
    }

    fn begin_provider_operation(&self) -> GraphExtractionProviderOperation {
        self.updates.send_modify(|state| {
            state.active_provider_operations = state.active_provider_operations.saturating_add(1);
        });
        GraphExtractionProviderOperation { activity: self.clone() }
    }

    fn record_chunk_completion(&self) {
        self.updates.send_modify(|state| {
            state.completed_chunks = state.completed_chunks.saturating_add(1);
        });
    }
}

impl Drop for GraphExtractionProviderOperation {
    fn drop(&mut self) {
        self.activity.updates.send_modify(|state| {
            state.active_provider_operations = state.active_provider_operations.saturating_sub(1);
        });
    }
}

impl GraphExtractionIdleWatchdog {
    pub(crate) fn new(idle_timeout: Duration) -> (GraphExtractionActivity, Self) {
        let (updates, receiver) = watch::channel(GraphExtractionActivityState::default());
        (GraphExtractionActivity { updates }, Self { idle_timeout, updates: receiver })
    }

    pub(crate) async fn wait_for_stall(mut self) {
        loop {
            let activity = *self.updates.borrow_and_update();
            if activity.active_provider_operations > 0 {
                if self.updates.changed().await.is_err() {
                    return;
                }
                continue;
            }

            tokio::select! {
                biased;
                changed = self.updates.changed() => {
                    if changed.is_err() {
                        tokio::time::sleep(self.idle_timeout).await;
                        return;
                    }
                }
                () = tokio::time::sleep(self.idle_timeout) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::GraphExtractionIdleWatchdog;

    const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

    #[tokio::test(start_paused = true)]
    async fn watchdog_is_suspended_during_provider_permit_wait() {
        let (activity, watchdog) = GraphExtractionIdleWatchdog::new(IDLE_TIMEOUT);
        let permit_wait = activity.begin_provider_operation();
        let stalled = tokio::spawn(watchdog.wait_for_stall());

        tokio::time::advance(IDLE_TIMEOUT.saturating_mul(3)).await;
        tokio::task::yield_now().await;
        assert!(!stalled.is_finished(), "provider permit wait must suspend the idle watchdog");

        drop(permit_wait);
        tokio::task::yield_now().await;
        tokio::time::advance(IDLE_TIMEOUT - Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(!stalled.is_finished(), "the full idle window starts after permit wait ends");

        tokio::time::advance(Duration::from_secs(1)).await;
        stalled.await.expect("watchdog task should finish after a true idle window");
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_fires_on_true_stall() {
        let (_activity, watchdog) = GraphExtractionIdleWatchdog::new(IDLE_TIMEOUT);
        let stalled = tokio::spawn(watchdog.wait_for_stall());
        tokio::task::yield_now().await;

        tokio::time::advance(IDLE_TIMEOUT).await;

        stalled.await.expect("watchdog task should report a true stall");
    }

    #[tokio::test(start_paused = true)]
    async fn chunk_completion_resets_watchdog() {
        let (activity, watchdog) = GraphExtractionIdleWatchdog::new(IDLE_TIMEOUT);
        let stalled = tokio::spawn(watchdog.wait_for_stall());
        tokio::task::yield_now().await;

        tokio::time::advance(IDLE_TIMEOUT - Duration::from_secs(1)).await;
        activity.record_chunk_completion();
        tokio::task::yield_now().await;
        tokio::time::advance(IDLE_TIMEOUT - Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(!stalled.is_finished(), "chunk completion must start a fresh idle window");

        tokio::time::advance(Duration::from_secs(1)).await;
        stalled.await.expect("watchdog task should finish after the reset idle window");
    }
}
