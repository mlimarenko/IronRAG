use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{Notify, broadcast};

#[derive(Clone, Debug)]
pub struct ShutdownSignal {
    state: Arc<ShutdownState>,
}

#[derive(Debug)]
struct ShutdownState {
    triggered: AtomicBool,
    notify: Notify,
    tx: broadcast::Sender<()>,
}

impl ShutdownSignal {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(8);
        Self {
            state: Arc::new(ShutdownState {
                triggered: AtomicBool::new(false),
                notify: Notify::new(),
                tx,
            }),
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.state.tx.subscribe()
    }

    pub async fn wait(&self) {
        while !self.is_triggered() {
            let notified = self.state.notify.notified();
            if self.is_triggered() {
                return;
            }
            notified.await;
        }
    }

    #[must_use]
    pub fn trigger(&self) -> bool {
        if self.state.triggered.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.state.notify.notify_waiters();
        let _ = self.state.tx.send(());
        true
    }

    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.state.triggered.load(Ordering::SeqCst)
    }
}

pub async fn wait_for_termination_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            let _ = tokio::signal::ctrl_c().await;
            return "SIGINT";
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "SIGINT",
            _ = terminate.recv() => "SIGTERM",
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::ShutdownSignal;

    #[tokio::test]
    async fn shutdown_signal_notifies_waiters_and_subscribers_once() {
        let signal = ShutdownSignal::new();
        let waiter = signal.clone();
        let waiter_task = tokio::spawn(async move {
            waiter.wait().await;
        });
        let mut subscriber = signal.subscribe();

        assert!(signal.trigger());
        assert!(!signal.trigger());

        timeout(Duration::from_secs(1), waiter_task)
            .await
            .expect("waiter completed within timeout")
            .expect("waiter task completed without panic");
        timeout(Duration::from_secs(1), subscriber.recv())
            .await
            .expect("subscriber received shutdown broadcast")
            .expect("shutdown broadcast delivered");
        assert!(signal.is_triggered());
    }
}
